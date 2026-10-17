//! Subscription LLM providers attached with an OAuth device-code sign-in.
//! Tokens stay in a 0600 file under /data/state/secrets and never leave through the web API.
//! Requests for an attached provider's models go to it directly; everything else uses the proxy.
use crate::llm::Llm;
use anyhow::{bail, Context, Result};
use reqwest::{header, Request, RequestBuilder, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Clone)]
pub(crate) struct Spec {
    pub slug: &'static str,
    pub name: &'static str,
    pub device_url: String,
    pub token_url: String,
    pub client_id: &'static str,
    pub scope: &'static str,
    pub base_url: String,
    /// Sign-in links shown to the operator must be on this domain.
    pub trusted_domain: &'static str,
}

// Public OAuth client of the upstream CLIs (see reefy-llm-proxy providers.py, openclaw xai-oauth.ts).
fn xai() -> Spec {
    Spec {
        slug: "xai",
        name: "xAI Grok",
        device_url: "https://auth.x.ai/oauth2/device/code".into(),
        token_url: "https://auth.x.ai/oauth2/token".into(),
        client_id: "b1a00492-073a-47ea-816f-4c329264a828",
        scope: "openid profile email offline_access grok-cli:access api:access",
        base_url: "https://api.x.ai/v1".into(),
        trusted_domain: "x.ai",
    }
}

impl Spec {
    fn check_link(&self, link: &str) -> Result<()> {
        let url = Url::parse(link).context("Untrusted sign-in link")?;
        let scheme = Url::parse(&self.device_url)?.scheme().to_owned();
        let host = url.host_str().unwrap_or_default();
        if url.scheme() != scheme
            || !url.username().is_empty()
            || url.password().is_some()
            || !(host == self.trusted_domain
                || host.ends_with(&format!(".{}", self.trusted_domain)))
        {
            bail!("Untrusted sign-in link")
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    attached_at: u64,
}

struct Pending {
    id: Uuid,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_at: u64,
}

#[derive(Default)]
struct State {
    credentials: HashMap<String, Credential>,
    pending: HashMap<String, Pending>,
    errors: HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    providers: HashMap<String, Credential>,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

pub(crate) struct Providers {
    path: Option<PathBuf>,
    specs: Vec<Spec>,
    state: Mutex<State>,
    // Refresh tokens rotate on use; concurrent 401s must share one refresh.
    refresh_lock: tokio::sync::Mutex<()>,
    client: reqwest::Client,
    min_interval: Duration,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 16384 && token.bytes().all(|b| b.is_ascii_graphic())
}

async fn read_json(mut response: Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            bail!("Sign-in response too large")
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn parse_credential(body: &Value, previous_refresh: Option<&str>) -> Result<Credential> {
    let access = body["access_token"].as_str().unwrap_or_default();
    let refresh = body["refresh_token"]
        .as_str()
        .or(previous_refresh)
        .unwrap_or_default();
    if !valid_token(access) || !valid_token(refresh) {
        bail!("Sign-in response is missing tokens")
    }
    let lifetime = body["expires_in"]
        .as_u64()
        .unwrap_or(3600)
        .clamp(60, 30 * 86400);
    Ok(Credential {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: now() + lifetime,
        attached_at: now(),
    })
}

impl Providers {
    pub(crate) fn memory() -> Self {
        Self::with_specs(None, vec![xai()])
    }
    pub(crate) fn load(path: PathBuf) -> Result<Self> {
        Self::load_with(path, vec![xai()])
    }
    fn load_with(path: PathBuf, specs: Vec<Spec>) -> Result<Self> {
        let providers = Self::with_specs(Some(path.clone()), specs);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let file: CredentialFile = serde_json::from_slice(&bytes)
                    .context("Invalid LLM provider credentials file")?;
                providers.state.lock().unwrap().credentials = file
                    .providers
                    .into_iter()
                    .filter(|(slug, _)| providers.spec(slug).is_some())
                    .collect();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(providers)
    }
    fn with_specs(path: Option<PathBuf>, specs: Vec<Spec>) -> Self {
        Self {
            path,
            specs,
            state: Mutex::new(State::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
            min_interval: Duration::from_secs(1),
        }
    }
    pub(crate) fn persistent(&self) -> bool {
        self.path.is_some()
    }
    pub(crate) fn spec(&self, slug: &str) -> Option<&Spec> {
        self.specs.iter().find(|s| s.slug == slug)
    }
    pub(crate) fn attached(&self, slug: &str) -> bool {
        self.state.lock().unwrap().credentials.contains_key(slug)
    }
    fn attached_slugs(&self) -> Vec<&'static str> {
        let state = self.state.lock().unwrap();
        self.specs
            .iter()
            .filter(|s| state.credentials.contains_key(s.slug))
            .map(|s| s.slug)
            .collect()
    }
    /// Atomic replace; callers hold the state lock so writes cannot reorder.
    fn persist(&self, credentials: &HashMap<String, Credential>) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let dir = path.parent().context("Missing secrets directory")?;
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".llm-providers-{}.tmp", Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(
                &json!({ "providers": credentials }),
            )?)?;
            file.sync_all()?;
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
    /// Operator-facing state. Never includes tokens or the device code.
    pub(crate) fn status(&self) -> Value {
        let state = self.state.lock().unwrap();
        let t = now();
        let rows: Vec<Value> = self
            .specs
            .iter()
            .map(|s| {
                let pending = state.pending.get(s.slug).map(|p| {
                    json!({
                        "user_code": p.user_code,
                        "verification_uri": p.verification_uri,
                        "verification_uri_complete": p.verification_uri_complete,
                        "expires_in": p.expires_at.saturating_sub(t),
                    })
                });
                json!({
                    "slug": s.slug,
                    "name": s.name,
                    "attached": state.credentials.contains_key(s.slug),
                    "attached_at": state.credentials.get(s.slug).map(|c| c.attached_at),
                    "pending": pending,
                    "error": state.errors.get(s.slug),
                })
            })
            .collect();
        json!({ "providers": rows })
    }
    pub(crate) async fn start_attach(self: &Arc<Self>, slug: &str) -> Result<Value> {
        let spec = self.spec(slug).context("Unknown provider")?.clone();
        let response = self
            .client
            .post(&spec.device_url)
            .header(header::ACCEPT, "application/json")
            .form(&[("client_id", spec.client_id), ("scope", spec.scope)])
            .send()
            .await
            .context("Sign-in service unreachable")?;
        if !response.status().is_success() {
            bail!("Sign-in service returned HTTP {}", response.status())
        }
        let device: DeviceResponse = serde_json::from_value(read_json(response).await?)
            .context("Unexpected sign-in response")?;
        spec.check_link(&device.verification_uri)?;
        if let Some(link) = &device.verification_uri_complete {
            spec.check_link(link)?;
        }
        if device.user_code.is_empty()
            || device.user_code.len() > 64
            || device.device_code.is_empty()
        {
            bail!("Unexpected sign-in response")
        }
        let id = Uuid::new_v4();
        let expires_at = now() + device.expires_in.unwrap_or(300).clamp(30, 1800);
        {
            let mut state = self.state.lock().unwrap();
            state.errors.remove(slug);
            state.pending.insert(
                spec.slug.into(),
                Pending {
                    id,
                    user_code: device.user_code,
                    verification_uri: device.verification_uri,
                    verification_uri_complete: device.verification_uri_complete,
                    expires_at,
                },
            );
        }
        let interval = Duration::from_secs(device.interval.unwrap_or(5)).max(self.min_interval);
        let this = self.clone();
        let code = device.device_code;
        tokio::spawn(async move { this.poll(spec, id, code, interval, expires_at).await });
        Ok(self.status())
    }
    fn current(&self, slug: &str, id: Uuid) -> bool {
        self.state.lock().unwrap().pending.get(slug).map(|p| p.id) == Some(id)
    }
    fn fail(&self, slug: &str, id: Uuid, message: &str) {
        let mut state = self.state.lock().unwrap();
        if state.pending.get(slug).map(|p| p.id) == Some(id) {
            state.pending.remove(slug);
            state.errors.insert(slug.into(), message.into());
        }
    }
    async fn poll(
        &self,
        spec: Spec,
        id: Uuid,
        code: String,
        mut interval: Duration,
        expires_at: u64,
    ) {
        let slug = spec.slug;
        loop {
            tokio::time::sleep(interval).await;
            if !self.current(slug, id) {
                return;
            }
            if now() >= expires_at {
                return self.fail(slug, id, "Sign-in code expired. Click Attach to try again.");
            }
            let Ok(response) = self
                .client
                .post(&spec.token_url)
                .header(header::ACCEPT, "application/json")
                .form(&[
                    ("grant_type", DEVICE_GRANT),
                    ("client_id", spec.client_id),
                    ("device_code", &code),
                ])
                .send()
                .await
            else {
                continue; // Network blip; keep polling until the code expires.
            };
            let status = response.status();
            let body = read_json(response).await.unwrap_or(Value::Null);
            if status.is_success() {
                let Ok(credential) = parse_credential(&body, None) else {
                    return self.fail(slug, id, "Sign-in finished without usable tokens.");
                };
                let mut state = self.state.lock().unwrap();
                if state.pending.get(slug).map(|p| p.id) != Some(id) {
                    return;
                }
                state.pending.remove(slug);
                state.credentials.insert(slug.into(), credential);
                if self.persist(&state.credentials).is_err() {
                    state.credentials.remove(slug);
                    state
                        .errors
                        .insert(slug.into(), "Could not save the sign-in.".into());
                }
                return;
            }
            match body["error"].as_str() {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += Duration::from_secs(5),
                Some("access_denied" | "authorization_denied") => {
                    return self.fail(slug, id, "Sign-in was denied.")
                }
                Some("expired_token") => {
                    return self.fail(slug, id, "Sign-in code expired. Click Attach to try again.")
                }
                _ if status.is_server_error() => {}
                _ => {
                    return self.fail(
                        slug,
                        id,
                        &format!("Sign-in failed (HTTP {})", status.as_u16()),
                    )
                }
            }
        }
    }
    /// Removes saved tokens and cancels a sign-in in progress.
    pub(crate) fn detach(&self, slug: &str) -> Result<Value> {
        self.spec(slug).context("Unknown provider")?;
        {
            let mut state = self.state.lock().unwrap();
            state.pending.remove(slug);
            state.errors.remove(slug);
            if state.credentials.remove(slug).is_some() {
                self.persist(&state.credentials)?;
            }
        }
        Ok(self.status())
    }
    async fn token(&self, slug: &str) -> Result<String> {
        let credential = self.state.lock().unwrap().credentials.get(slug).cloned();
        let credential = credential.context("Provider not attached")?;
        if credential.expires_at > now() + 60 {
            return Ok(credential.access_token);
        }
        self.refresh(slug, &credential.access_token).await
    }
    async fn refresh(&self, slug: &str, stale: &str) -> Result<String> {
        let _guard = self.refresh_lock.lock().await;
        let credential = self.state.lock().unwrap().credentials.get(slug).cloned();
        let credential = credential.context("Provider not attached")?;
        if credential.access_token != stale && credential.expires_at > now() + 60 {
            return Ok(credential.access_token); // Another request already refreshed.
        }
        let spec = self.spec(slug).context("Unknown provider")?;
        let response = self
            .client
            .post(&spec.token_url)
            .header(header::ACCEPT, "application/json")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", credential.refresh_token.as_str()),
                ("client_id", spec.client_id),
            ])
            .send()
            .await
            .context("Sign-in service unreachable")?;
        let status = response.status();
        if status.is_server_error() {
            bail!("Sign-in service temporarily unavailable")
        }
        if !status.is_success() {
            // A rejected refresh token is spent; only a new sign-in can recover.
            let mut state = self.state.lock().unwrap();
            if state
                .credentials
                .get(slug)
                .is_some_and(|c| c.refresh_token == credential.refresh_token)
            {
                state.credentials.remove(slug);
                state.errors.insert(
                    slug.into(),
                    "Sign-in expired. Click Attach to sign in again.".into(),
                );
                let _ = self.persist(&state.credentials);
            }
            bail!("Provider sign-in expired")
        }
        let mut next =
            parse_credential(&read_json(response).await?, Some(&credential.refresh_token))?;
        next.attached_at = credential.attached_at;
        let token = next.access_token.clone();
        let mut state = self.state.lock().unwrap();
        state.credentials.insert(slug.into(), next);
        self.persist(&state.credentials)?;
        Ok(token)
    }
}

fn set_bearer(request: &mut Request, token: &str) -> Result<()> {
    let mut value = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
    value.set_sensitive(true);
    request.headers_mut().insert(header::AUTHORIZATION, value);
    Ok(())
}

impl Llm {
    /// Sends a request built for the proxy, rerouting it to an attached provider when the
    /// body names one of its models (`xai/...`). xAI-only multipart endpoints follow xAI.
    pub(crate) async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let request = request.build()?;
        let slug = self.direct_provider(&request);
        self.dispatch(slug, request).await
    }
    fn direct_provider(&self, request: &Request) -> Option<&'static str> {
        let suffix = request.url().as_str().strip_prefix(&self.base)?;
        let model = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice::<Value>(b).ok())
            .and_then(|v| v["model"].as_str().map(str::to_owned));
        let slug = match &model {
            Some(model) => model.split_once('/')?.0,
            None if suffix == "/stt" || suffix == "/files" || suffix.starts_with("/files/") => {
                "xai"
            }
            None => return None,
        };
        let spec = self.providers.spec(slug)?;
        self.providers.attached(spec.slug).then_some(spec.slug)
    }
    async fn dispatch(&self, slug: Option<&'static str>, mut request: Request) -> Result<Response> {
        let Some(slug) = slug else {
            return Ok(self.client.execute(request).await?);
        };
        let spec = self.providers.spec(slug).context("Unknown provider")?;
        let suffix = request
            .url()
            .as_str()
            .strip_prefix(&self.base)
            .context("Unexpected model endpoint")?
            .to_owned();
        *request.url_mut() = Url::parse(&format!("{}{suffix}", spec.base_url))?;
        let body = request
            .body()
            .and_then(|b| b.as_bytes())
            .map(<[u8]>::to_vec);
        if let Some(bytes) = body {
            if let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) {
                let bare = value["model"]
                    .as_str()
                    .and_then(|m| m.strip_prefix(&format!("{slug}/")))
                    .map(str::to_owned);
                if let Some(bare) = bare {
                    value["model"] = json!(bare);
                    *request.body_mut() = Some(serde_json::to_vec(&value)?.into());
                }
            }
        }
        let token = self.providers.token(slug).await?;
        set_bearer(&mut request, &token)?;
        let retry = request.try_clone();
        let response = self.client.execute(request).await?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        // Lazy refresh, as in reefy-llm-proxy: retry once with a rotated token.
        let Some(mut retry) = retry else {
            return Ok(response);
        };
        let token = self.providers.refresh(slug, &token).await?;
        set_bearer(&mut retry, &token)?;
        Ok(self.client.execute(retry).await?)
    }
    /// Models from attached providers (prefixed with their slug) plus the proxy's models.
    pub(crate) async fn direct_models(&self) -> (Vec<String>, Option<anyhow::Error>) {
        let mut ids = Vec::new();
        let mut error = None;
        for slug in self.providers.attached_slugs() {
            let request = self
                .client
                .get(format!("{}/models", self.base))
                .timeout(Duration::from_secs(15));
            let result = async {
                let request = request.build()?;
                let value = self
                    .decode(self.dispatch(Some(slug), request).await?)
                    .await?;
                let models = value["data"]
                    .as_array()
                    .context("Invalid models response")?;
                Ok::<_, anyhow::Error>(
                    models
                        .iter()
                        .filter_map(|v| v["id"].as_str())
                        .filter(|v| !v.is_empty() && v.len() <= 200)
                        .map(|v| format!("{slug}/{v}"))
                        .collect::<Vec<_>>(),
                )
            }
            .await;
            match result {
                Ok(models) => ids.extend(models),
                Err(e) => error = Some(e),
            }
        }
        (ids, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Form, Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn serve(app: impl FnOnce(String) -> Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = app(base.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }
    fn spec(base: &str) -> Spec {
        Spec {
            slug: "xai",
            name: "xAI Grok",
            device_url: format!("{base}/device"),
            token_url: format!("{base}/token"),
            client_id: "test-client",
            scope: "test-scope",
            base_url: format!("{base}/api"),
            trusted_domain: "127.0.0.1",
        }
    }
    fn device(base: &str) -> Value {
        json!({"device_code":"DEVICE-SECRET","user_code":"ABCD-1234","verification_uri":format!("{base}/activate"),
            "verification_uri_complete":format!("{base}/activate?code=ABCD-1234"),"expires_in":60,"interval":0})
    }
    async fn wait_until(providers: &Providers, done: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..200 {
            let status = providers.status();
            if done(&status["providers"][0]) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out: {}", providers.status())
    }

    #[tokio::test]
    async fn device_login_saves_tokens_privately_and_hides_them() {
        let polls = Arc::new(AtomicUsize::new(0));
        let seen = polls.clone();
        let base = serve(move |base| {
            let body = device(&base);
            Router::new()
                .route("/device", post(move |Form(f): Form<HashMap<String, String>>| {
                    assert_eq!(f["client_id"], "test-client");
                    assert_eq!(f["scope"], "test-scope");
                    let body = body.clone();
                    async move { Json(body) }
                }))
                .route("/token", post(move |Form(f): Form<HashMap<String, String>>| {
                    assert_eq!(f["grant_type"], DEVICE_GRANT);
                    assert_eq!(f["device_code"], "DEVICE-SECRET");
                    let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        if first {
                            (StatusCode::BAD_REQUEST, Json(json!({"error":"authorization_pending"})))
                        } else {
                            (StatusCode::OK, Json(json!({"access_token":"ACCESS-1","refresh_token":"REFRESH-1","expires_in":3600})))
                        }
                    }
                }))
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets/llm-providers.json");
        let mut providers = Providers::with_specs(Some(path.clone()), vec![spec(&base)]);
        providers.min_interval = Duration::from_millis(5);
        let providers = Arc::new(providers);
        let started = providers.start_attach("xai").await.unwrap();
        let row = &started["providers"][0];
        assert_eq!(row["pending"]["user_code"], "ABCD-1234");
        assert_eq!(row["attached"], false);
        assert!(!started.to_string().contains("DEVICE-SECRET"));
        let done = wait_until(&providers, |p| p["attached"] == true).await;
        assert!(done["providers"][0]["pending"].is_null());
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        let text = done.to_string();
        assert!(!text.contains("ACCESS-1") && !text.contains("REFRESH-1"));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("ACCESS-1") && saved.contains("REFRESH-1"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let reloaded = Providers::load_with(path.clone(), vec![spec(&base)]).unwrap();
        assert!(reloaded.attached("xai"));
        providers.detach("xai").unwrap();
        assert!(!providers.attached("xai"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("ACCESS-1"));
    }

    #[tokio::test]
    async fn denied_login_and_untrusted_links_attach_nothing() {
        let base = serve(|base| {
            let body = device(&base);
            Router::new()
                .route(
                    "/device",
                    post(move || {
                        let body = body.clone();
                        async move { Json(body) }
                    }),
                )
                .route(
                    "/token",
                    post(|| async {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error":"access_denied"})),
                        )
                    }),
                )
        })
        .await;
        let mut providers = Providers::with_specs(None, vec![spec(&base)]);
        providers.min_interval = Duration::from_millis(5);
        let providers = Arc::new(providers);
        providers.start_attach("xai").await.unwrap();
        let done = wait_until(&providers, |p| !p["error"].is_null()).await;
        assert_eq!(done["providers"][0]["error"], "Sign-in was denied.");
        assert_eq!(done["providers"][0]["attached"], false);

        let evil = serve(|_| {
            Router::new().route("/device", post(|| async {
                Json(json!({"device_code":"d","user_code":"u","verification_uri":"javascript:alert(1)"}))
            }))
        })
        .await;
        let providers = Arc::new(Providers::with_specs(None, vec![spec(&evil)]));
        assert!(providers.start_attach("xai").await.is_err());
        assert!(providers.status()["providers"][0]["pending"].is_null());
        assert!(providers.start_attach("other").await.is_err());
    }

    fn saved(path: &std::path::Path, access: &str, refresh: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, json!({"providers":{"xai":{"access_token":access,"refresh_token":refresh,"expires_at":now()+3600,"attached_at":1}}}).to_string()).unwrap();
    }

    #[tokio::test]
    async fn attached_models_go_direct_with_refresh_and_others_use_proxy() {
        let direct_calls = Arc::new(AtomicUsize::new(0));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let (d, r) = (direct_calls.clone(), refreshes.clone());
        let xai = serve(move |_| {
            Router::new()
                .route("/api/chat/completions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    d.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if headers["authorization"] != "Bearer NEW" {
                            return (StatusCode::UNAUTHORIZED, Json(json!({})));
                        }
                        assert_eq!(body["model"], "grok-test");
                        (StatusCode::OK, Json(json!({"choices":[{"message":{"role":"assistant","content":"direct"}}]})))
                    }
                }))
                .route("/api/models", get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer NEW");
                    Json(json!({"data":[{"id":"grok-test"}]}))
                }))
                .route("/token", post(move |Form(f): Form<HashMap<String, String>>| {
                    r.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(f["grant_type"], "refresh_token");
                    assert_eq!(f["refresh_token"], "R-OLD");
                    async { Json(json!({"access_token":"NEW","refresh_token":"R-NEW","expires_in":3600})) }
                }))
        })
        .await;
        let proxy_calls = Arc::new(AtomicUsize::new(0));
        let p = proxy_calls.clone();
        let proxy = serve(move |_| {
            Router::new()
                .route("/v1/chat/completions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    p.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(headers["authorization"], "Bearer proxy-key");
                    assert_eq!(body["model"], "other/model");
                    async { Json(json!({"choices":[{"message":{"role":"assistant","content":"proxy"}}]})) }
                }))
                .route("/v1/models", get(|| async { Json(json!({"data":[{"id":"other/model"}]})) }))
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm-providers.json");
        saved(&path, "OLD", "R-OLD");
        let mut llm = Llm::new(format!("{proxy}/v1"), "proxy-key".into()).unwrap();
        llm.providers = Arc::new(Providers::load_with(path.clone(), vec![spec(&xai)]).unwrap());
        let ask = |model: &str| {
            llm.client
                .post(format!("{}/chat/completions", llm.base))
                .bearer_auth(&llm.key)
                .json(&json!({"model":model,"messages":[]}))
        };
        let value = llm
            .decode(llm.send(ask("xai/grok-test")).await.unwrap())
            .await
            .unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "direct");
        assert_eq!(
            (
                direct_calls.load(Ordering::SeqCst),
                refreshes.load(Ordering::SeqCst)
            ),
            (2, 1)
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("R-NEW"));
        let value = llm
            .decode(llm.send(ask("other/model")).await.unwrap())
            .await
            .unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "proxy");
        assert_eq!(proxy_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            llm.models().await.unwrap(),
            ["other/model", "xai/grok-test"]
        );
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejected_refresh_detaches_and_proxy_outage_keeps_direct_models() {
        let xai = serve(|_| {
            Router::new()
                .route(
                    "/api/chat/completions",
                    post(|| async { (StatusCode::UNAUTHORIZED, Json(json!({}))) }),
                )
                .route(
                    "/api/models",
                    get(|| async { Json(json!({"data":[{"id":"grok-test"}]})) }),
                )
                .route(
                    "/token",
                    post(|| async {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error":"invalid_grant"})),
                        )
                    }),
                )
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm-providers.json");
        saved(&path, "OLD", "R-OLD");
        // Nothing listens on port 9; the proxy is unreachable.
        let mut llm = Llm::new("http://127.0.0.1:9/v1".into(), "unused".into()).unwrap();
        llm.providers = Arc::new(Providers::load_with(path.clone(), vec![spec(&xai)]).unwrap());
        assert_eq!(llm.models().await.unwrap(), ["xai/grok-test"]);
        let request = llm
            .client
            .post(format!("{}/chat/completions", llm.base))
            .json(&json!({"model":"xai/grok-test"}));
        assert!(llm.send(request).await.is_err());
        let status = llm.providers.status();
        assert_eq!(status["providers"][0]["attached"], false);
        assert!(status["providers"][0]["error"]
            .as_str()
            .unwrap()
            .contains("Attach"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("R-OLD"));
        assert!(llm.models().await.is_err());
    }
}
