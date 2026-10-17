mod attachment_llm;
mod concurrency;
mod contacts;
mod coordination;
mod delivery;
mod document_sharing;
mod document_summary;
mod execution;
mod ingress;
mod inquiry_routing;
mod llm;
mod media;
mod message_notes;
mod permissions;
mod private_context;
mod profiles;
mod progress;
mod revisions;
mod runtime;
mod scoped_resources;
mod search;
mod store;
mod tasks;
mod trace;
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

fn default_user_limit() -> i64 {
    50
}
fn default_global_limit() -> i64 {
    500
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen: SocketAddr,
    auth: AuthConfig,
    #[serde(default = "default_user_limit")]
    daily_user_limit: i64,
    #[serde(default = "default_global_limit")]
    daily_global_limit: i64,
    #[serde(default)]
    experimental_tasks: bool,
    #[serde(default)]
    debug_trace: bool,
    #[serde(default = "default_model_concurrency")]
    model_concurrency: usize,
}
fn default_model_concurrency() -> usize {
    4
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum AuthConfig {
    Basic { password_file: PathBuf },
    OptionalBasic { password_file: PathBuf },
    Disabled,
}

struct AppState {
    credential_digest: Option<[u8; 32]>,
    failures: Mutex<HashMap<std::net::IpAddr, (u8, Instant)>>,
    runtime: Option<Arc<runtime::Runtime>>,
}

fn load_config(path: &str) -> Result<(Config, AppState), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 65536 {
        return Err("Configuration exceeds 64 KiB".into());
    }
    let config: Config = serde_json::from_slice(&bytes)?;
    if !(1..=10000).contains(&config.daily_user_limit)
        || !(1..=100000).contains(&config.daily_global_limit)
    {
        return Err("Invalid request limits".into());
    }
    if !(1..=16).contains(&config.model_concurrency) {
        return Err("model_concurrency must be between 1 and 16".into());
    }
    let credential_digest = match &config.auth {
        AuthConfig::Disabled => {
            if !config.listen.ip().is_loopback() {
                return Err("Disabled authentication requires a loopback listener".into());
            }
            None
        }
        AuthConfig::OptionalBasic { password_file } => optional_password_digest(password_file)?,
        AuthConfig::Basic { password_file } => {
            let password = std::fs::read_to_string(password_file)?;
            let password = password.trim_end_matches(['\r', '\n']);
            if password.len() < 16 || password.len() > 1024 {
                return Err("Password must contain 16–1024 bytes".into());
            }
            Some(Sha256::digest(format!("admin:{password}").as_bytes()).into())
        }
    };
    Ok((
        config,
        AppState {
            credential_digest,
            failures: Mutex::new(HashMap::new()),
            runtime: None,
        },
    ))
}

// Matches CamAdmiral: absence disables optional Basic auth; other errors fail closed.
fn optional_password_digest(
    path: &std::path::Path,
) -> Result<Option<[u8; 32]>, Box<dyn std::error::Error>> {
    let password = match std::fs::read_to_string(path) {
        Ok(password) => password,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let password = password.trim();
    if password.is_empty() || password.len() > 1024 {
        return Err("Optional admin password is empty or too long".into());
    }
    Ok(Some(
        Sha256::digest(format!("admin:{password}").as_bytes()).into(),
    ))
}

async fn protect(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if let Some(expected) = state.credential_digest {
        let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied()
        else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let now = Instant::now();
        let blocked = {
            let mut failures = state.failures.lock().unwrap();
            failures.retain(|_, (_, since)| now.duration_since(*since) < Duration::from_secs(300));
            failures
                .get(&peer.ip())
                .is_some_and(|(count, _)| *count >= 5)
        };
        if blocked {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "300")],
            )
                .into_response();
        }
        let valid = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.split_once(' '))
            .filter(|(scheme, value)| scheme.eq_ignore_ascii_case("basic") && value.len() <= 2048)
            .and_then(|(_, value)| STANDARD.decode(value).ok())
            .map(|value| {
                let candidate: [u8; 32] = Sha256::digest(value).into();
                bool::from(expected.ct_eq(&candidate))
            })
            .unwrap_or(false);
        if !valid {
            let mut failures = state.failures.lock().unwrap();
            if failures.len() < 4096 || failures.contains_key(&peer.ip()) {
                let record = failures.entry(peer.ip()).or_insert((0, now));
                record.0 = record.0.saturating_add(1);
            } else {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
            return (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"Carbon\", charset=\"UTF-8\"",
                )],
            )
                .into_response();
        }
        state.failures.lock().unwrap().remove(&peer.ip());
    }
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        // Browsers cannot send this header cross-origin without a CORS preflight.
        // No CORS permission is granted. Basic credentials alone are insufficient.
        if req
            .headers()
            .get("x-carbon-request")
            .and_then(|h| h.to_str().ok())
            != Some("1")
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(req).await
}

async fn headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    ] { response.headers_mut().insert(axum::http::HeaderName::from_static(name), value.parse().unwrap()); }
    response
}

fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/logo.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../web/logo.png").as_slice(),
                )
            }),
        )
        .route(
            "/favicon.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../web/favicon.png").as_slice(),
                )
            }),
        )
        .route(
            "/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../web/app.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/api/status",
            get(|State(s): State<Arc<AppState>>| async move {
                Json(
                    s.runtime
                        .as_ref()
                        .map(|r| r.status())
                        .unwrap_or_else(|| json!({"whatsapp":"Not implemented"})),
                )
            }),
        )
        .route("/api/models", get(models))
        .route("/api/activity", get(activity))
        .route("/api/traces/:operation", get(trace_export))
        .route("/api/approval-requests", get(approval_requests))
        .route("/api/model", post(set_model))
        .route("/api/participants", get(participants))
        .route("/api/admins/bootstrap", post(bootstrap))
        .route("/api/admins/requests", post(request_admin))
        .route("/api/whatsapp/qr.svg", get(qr))
        .route("/api/whatsapp/pair-code", post(pair_code))
        .route("/api/whatsapp/pairing-view", post(pairing_view))
        .route("/api/whatsapp/unlink", post(unlink_whatsapp))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn_with_state(state.clone(), protect))
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn(headers))
        .with_state(state)
}

async fn approval_requests(State(s): State<Arc<AppState>>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.approval_requests().await {
        Ok(data) => Json(data).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn trace_export(
    State(s): State<Arc<AppState>>,
    axum::extract::Path(operation): axum::extract::Path<String>,
) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if uuid::Uuid::parse_str(&operation).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match rt.store.trace_export(&operation) {
        Ok(data) => (
            [
                (header::CACHE_CONTROL, "no-store"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=carbon-trace.json",
                ),
            ],
            Json(data),
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn activity(State(s): State<Arc<AppState>>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.activity_rows().await {
        Ok(events) => Json(events).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn models(State(s): State<Arc<AppState>>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.llm.models().await {
        Ok(models) => Json(json!({"models":models})).into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "Cannot reach Reefy LLM proxy").into_response(),
    }
}
async fn set_model(
    State(s): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(model) = body["model"].as_str() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match rt.llm.models().await {
        Ok(models) if models.iter().any(|m| m == model) => match rt.store.set_model(model) {
            Ok(_) => {
                rt.store.log(
                    "info",
                    "model_selected",
                    None,
                    "Language model selection saved",
                );
                StatusCode::NO_CONTENT.into_response()
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(_) => (
            StatusCode::BAD_REQUEST,
            "Choose a model reported by the proxy",
        )
            .into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}
async fn participants(State(s): State<Arc<AppState>>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.participants().await {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminRequest {
    user: uuid::Uuid,
}
async fn request_admin(
    State(s): State<Arc<AppState>>,
    Json(request): Json<AdminRequest>,
) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.store.propose_web_admin(request.user.to_string()) {
        Ok(value) => Json(value).into_response(),
        Err(e) => (StatusCode::CONFLICT, e.to_string()).into_response(),
    }
}
async fn bootstrap(State(s): State<Arc<AppState>>, Json(ids): Json<Vec<String>>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.store.bootstrap_admins(&ids) {
        Ok(_) => {
            rt.store
                .log("info", "admins_sealed", None, "Initial admins sealed");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::CONFLICT, e.to_string()).into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingView {
    viewer: uuid::Uuid,
    open: bool,
}
async fn pairing_view(State(s): State<Arc<AppState>>, Json(view): Json<PairingView>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match rt.set_pairing_viewer(view.viewer, view.open) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::TOO_MANY_REQUESTS.into_response(),
    }
}
async fn unlink_whatsapp(State(s): State<Arc<AppState>>) -> Response {
    use std::sync::atomic::Ordering;
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if rt.client.lock().unwrap().is_none() && !rt.relink_required() {
        return (StatusCode::CONFLICT, "WhatsApp is not linked.").into_response();
    }
    if rt
        .unlinking
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return (StatusCode::CONFLICT, "Unlinking is already in progress.").into_response();
    }
    let rt = rt.clone();
    // Continue even if the phone closes the tab during logout.
    tokio::spawn(async move {
        if rt.unlink().await.is_err() {
            *rt.client.lock().unwrap() = None;
            *rt.status.lock().unwrap() = "Unlink failed. Restart Carbon to retry linking.".into();
            rt.store.log(
                "error",
                "unlink_failed",
                None,
                "WhatsApp unlink did not finish; restart required",
            );
        }
        rt.unlinking.store(false, Ordering::SeqCst);
    });
    (StatusCode::ACCEPTED, Json(json!({"status":"unlinking"}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairCodeRequest {
    phone_number: String,
}
async fn pair_code(State(s): State<Arc<AppState>>, Json(body): Json<PairCodeRequest>) -> Response {
    let Some(phone) = runtime::normalize_phone(&body.phone_number) else {
        return (
            StatusCode::BAD_REQUEST,
            "Enter your phone number with country code (7-15 digits).",
        )
            .into_response();
    };
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if rt.unlinking.load(std::sync::atomic::Ordering::SeqCst) {
        return (StatusCode::CONFLICT, "Unlinking is in progress.").into_response();
    }
    if rt.relink_required() {
        return (
            StatusCode::CONFLICT,
            "Relink WhatsApp before requesting a linking code.",
        )
            .into_response();
    }
    let Ok(mut pairing) = rt.pairing.try_lock() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "A linking request is in progress.",
        )
            .into_response();
    };
    if rt.client.lock().unwrap().is_some() {
        return (StatusCode::CONFLICT, "WhatsApp is already linked.").into_response();
    }
    let now = Instant::now();
    if let Some((number, code, expiry)) = &pairing.active {
        if *expiry > now {
            return if number == &phone {
                Json(json!({"code":code,"expires_in":(*expiry-now).as_secs()})).into_response()
            } else {
                (
                    StatusCode::CONFLICT,
                    "A code is active for another number. Wait for it to expire.",
                )
                    .into_response()
            };
        }
    }
    pairing.active = None;
    if pairing.retry_after.is_some_and(|expiry| expiry > now) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait before requesting another code.",
        )
            .into_response();
    }
    // A live QR means the unpaired socket has completed its initial handshake.
    if !rt
        .qr
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(_, expiry)| *expiry > now)
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "WhatsApp is not ready to link. Try again shortly.",
        )
            .into_response();
    }
    let Some(client) = rt.pairing_client.lock().unwrap().clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    pairing.retry_after = Some(now + Duration::from_secs(60));
    let options = whatsapp_rust::pair_code::PairCodeOptions {
        phone_number: phone.clone(),
        show_push_notification: true,
        ..Default::default()
    };
    rt.store.log(
        "info",
        "pair_code_requested",
        None,
        "Requesting phone linking code",
    );
    match tokio::time::timeout(Duration::from_secs(45), client.pair_with_code(options)).await {
        Ok(Ok(code)) => {
            rt.store
                .log("info", "pair_code_ready", None, "Phone linking code issued");
            // whatsapp-rust 0.7 codes expire 180 seconds after request initiation.
            let expiry = now + Duration::from_secs(180);
            pairing.active = Some((phone, code.clone(), expiry));
            Json(json!({"code":code,"expires_in":expiry.saturating_duration_since(Instant::now()).as_secs()})).into_response()
        }
        Ok(Err(error)) => {
            rt.store.log(
                "warn",
                "pair_code_failed",
                None,
                "WhatsApp could not issue a linking code",
            );
            if let Some(backoff) = error.backoff() {
                pairing.retry_after = Some(Instant::now() + backoff.max(Duration::from_secs(60)));
            }
            (StatusCode::BAD_GATEWAY, "WhatsApp could not issue a code. Check the number and retry later, or use QR linking.").into_response()
        }
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            "WhatsApp did not respond. Wait a minute and try again.",
        )
            .into_response(),
    }
}

async fn qr(State(s): State<Arc<AppState>>) -> Response {
    let Some(rt) = &s.runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let qr = rt.qr.lock().unwrap();
    match qr.as_ref().filter(|(_, expiry)| *expiry > Instant::now()) {
        Some((code, _)) => match qrcode::QrCode::new(code.as_bytes()) {
            Ok(qr) => (
                [(header::CONTENT_TYPE, "image/svg+xml")],
                qr.render::<qrcode::render::svg::Color>()
                    .min_dimensions(280, 280)
                    .build(),
            )
                .into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "carbon-web.json".into());
    let (config, mut state) = load_config(&path)?;
    let root = PathBuf::from(std::env::var("CARBON_DATA_DIR").unwrap_or_else(|_| "/data".into()));
    let runtime = runtime::Runtime::open(&root, config.daily_user_limit, config.daily_global_limit)
        .map_err(|e| e.to_string())?;
    runtime.store.db.lock().unwrap().execute(
        "INSERT OR REPLACE INTO settings VALUES('experimental_tasks',?)",
        [if config.experimental_tasks { "1" } else { "0" }],
    )?;
    runtime.store.db.lock().unwrap().execute(
        "INSERT OR REPLACE INTO settings VALUES('debug_trace',?)",
        [if config.debug_trace { "1" } else { "0" }],
    )?;
    runtime.store.log("info", "app_started", None, "Carbon started");
    state.runtime = Some(runtime.clone());
    runtime.start().await.map_err(|e| e.to_string())?;
    let worker = tokio::spawn(runtime.clone().worker(config.model_concurrency));
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    eprintln!("Carbon setup UI listening on {}", listener.local_addr()?);
    axum::serve(
        listener,
        app(Arc::new(state)).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    runtime.store.prepare_execution_restart()?;
    worker.abort();
    let _ = worker.await;
    runtime
        .store
        .log("info", "app_stopping", None, "Carbon stopping");
    runtime.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn test_app() -> Router {
        app(Arc::new(AppState {
            credential_digest: Some(Sha256::digest(b"admin:test-password-long").into()),
            failures: Mutex::new(HashMap::new()),
            runtime: None,
        }))
    }
    fn request(path: &str, password: Option<&str>) -> Request {
        let mut req = Request::builder().uri(path);
        if let Some(password) = password {
            req = req.header(
                header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode(format!("admin:{password}"))),
            );
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:32100".parse::<SocketAddr>().unwrap(),
        ));
        req
    }
    #[tokio::test]
    async fn web_admin_request_route_enforces_guards_and_only_stages() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime::Runtime::open(dir.path(), 50, 500).unwrap();
        rt.store.ingest("admin@test", "a", "hello").unwrap();
        let admin = rt.store.next().unwrap().unwrap();
        rt.store.ingest("candidate@test", "b", "hello").unwrap();
        let candidate = rt.store.next().unwrap().unwrap();
        rt.store.bootstrap_admins(&[admin.user]).unwrap();
        let router = app(Arc::new(AppState {
            credential_digest: Some(Sha256::digest(b"admin:test-password-long").into()),
            failures: Mutex::new(HashMap::new()),
            runtime: Some(rt.clone()),
        }));
        for (authorized, marked, extra, expected) in [
            (false, true, false, StatusCode::UNAUTHORIZED),
            (true, false, false, StatusCode::FORBIDDEN),
            (true, true, true, StatusCode::UNPROCESSABLE_ENTITY),
            (true, true, false, StatusCode::OK),
        ] {
            let mut req = request(
                "/api/admins/requests",
                authorized.then_some("test-password-long"),
            );
            *req.method_mut() = axum::http::Method::POST;
            req.headers_mut()
                .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
            if marked {
                req.headers_mut()
                    .insert("X-Carbon-Request", "1".parse().unwrap());
            }
            let mut body = json!({"user":candidate.user});
            if extra {
                body["op"] = json!("create_group");
            }
            *req.body_mut() = Body::from(body.to_string());
            assert_eq!(
                router.clone().oneshot(req).await.unwrap().status(),
                expected
            );
        }
        assert_eq!(
            rt.store.permission_directory(&candidate.user).unwrap()["can_manage_approval_admins"],
            false
        );
        assert!(rt
            .store
            .claim_control()
            .unwrap()
            .unwrap()
            .2
            .contains("Requested from Carbon web UI"));
    }
    #[tokio::test]
    async fn trace_download_route_is_authenticated_and_matches_uuid() {
        let path = "/api/traces/00000000-0000-0000-0000-000000000000";
        assert_eq!(
            test_app()
                .oneshot(request(path, None))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            test_app()
                .oneshot(request(path, Some("test-password-long")))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    #[tokio::test]
    async fn every_management_route_requires_auth() {
        for path in [
            "/",
            "/app.js",
            "/app.css",
            "/api/status",
            "/api/activity",
            "/api/approval-requests",
            "/api/admins/requests",
            "/api/unknown",
        ] {
            let response = test_app().oneshot(request(path, None)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        assert_eq!(
            test_app()
                .oneshot(request("/healthz", None))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    #[tokio::test]
    async fn authenticated_status_has_no_secrets_or_fabricated_connections() {
        let response = test_app()
            .oneshot(request("/api/status", Some("test-password-long")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["whatsapp"], "Not implemented");
        assert!(!String::from_utf8_lossy(&body).contains("test-password"));
    }
    #[tokio::test]
    async fn repeated_failures_lock_out_client() {
        let router = test_app();
        for _ in 0..5 {
            assert_eq!(
                router
                    .clone()
                    .oneshot(request("/api/status", Some("wrong")))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            router
                .oneshot(request("/api/status", Some("test-password-long")))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    #[test]
    fn configuration_fails_closed() {
        let path = std::env::temp_dir().join(format!("carbon-config-test-{}.json", std::process::id()));
        for config in [
            r#"{"listen":"0.0.0.0:18080","auth":{"mode":"disabled"}}"#,
            r#"{"listen":"127.0.0.1:18080","auth":{"mode":"basic","password_file":"/nonexistent/carbon-secret"}}"#,
            r#"{"listen":"127.0.0.1:18080","auth":{"mode":"disabled"},"typo":true}"#,
        ] {
            std::fs::write(&path, config).unwrap();
            assert!(load_config(path.to_str().unwrap()).is_err());
        }
        std::fs::write(
            &path,
            r#"{"listen":"127.0.0.1:18080","auth":{"mode":"disabled"}}"#,
        )
        .unwrap();
        assert!(load_config(path.to_str().unwrap()).is_ok());
        std::fs::remove_file(path).unwrap();
    }
    #[tokio::test]
    async fn mutations_require_explicit_same_origin_header() {
        let mut req = request("/api/admins/bootstrap", Some("test-password-long"));
        *req.method_mut() = axum::http::Method::POST;
        assert_eq!(
            test_app().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        for path in ["/api/models", "/api/participants", "/api/whatsapp/qr.svg"] {
            assert_eq!(
                test_app()
                    .oneshot(request(path, None))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
    #[tokio::test]
    async fn pairing_view_requires_auth_and_mutation_header() {
        for (auth, marked, expected) in [
            (false, true, StatusCode::UNAUTHORIZED),
            (true, false, StatusCode::FORBIDDEN),
        ] {
            let mut req = request(
                "/api/whatsapp/pairing-view",
                auth.then_some("test-password-long"),
            );
            *req.method_mut() = axum::http::Method::POST;
            if marked {
                req.headers_mut()
                    .insert("X-Carbon-Request", "1".parse().unwrap());
            }
            assert_eq!(test_app().oneshot(req).await.unwrap().status(), expected);
        }
    }
    #[tokio::test]
    async fn unlink_requires_auth_and_mutation_header() {
        for (authorized, marked, expected) in [
            (false, true, StatusCode::UNAUTHORIZED),
            (true, false, StatusCode::FORBIDDEN),
            (true, true, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let mut req = request(
                "/api/whatsapp/unlink",
                authorized.then_some("test-password-long"),
            );
            *req.method_mut() = axum::http::Method::POST;
            if marked {
                req.headers_mut()
                    .insert("X-Carbon-Request", "1".parse().unwrap());
            }
            assert_eq!(test_app().oneshot(req).await.unwrap().status(), expected);
        }
    }
    fn pairing_request(phone: &str, authorized: bool, marked: bool) -> Request {
        let mut req = request(
            "/api/whatsapp/pair-code",
            authorized.then_some("test-password-long"),
        );
        *req.method_mut() = axum::http::Method::POST;
        req.headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        if marked {
            req.headers_mut()
                .insert("X-Carbon-Request", "1".parse().unwrap());
        }
        *req.body_mut() = Body::from(serde_json::to_vec(&json!({"phone_number":phone})).unwrap());
        req
    }
    #[tokio::test]
    async fn revoked_link_can_be_reset_through_authenticated_api() {
        let dir = tempfile::tempdir().unwrap();
        let store = store::Store::open(dir.path()).unwrap();
        store
            .record_whatsapp_logout("LoggedOut; on_connect=true")
            .unwrap();
        store.set_model("preserved-model").unwrap();
        drop(store);
        let rt = runtime::Runtime::open(dir.path(), 50, 500).unwrap();
        let router = app(Arc::new(AppState {
            credential_digest: Some(Sha256::digest(b"admin:test-password-long").into()),
            failures: Mutex::new(HashMap::new()),
            runtime: Some(rt.clone()),
        }));
        assert_eq!(
            router
                .clone()
                .oneshot(pairing_request("12025550123", true, true))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        // A revoked link remains protected by the existing authentication and
        // same-origin mutation checks, despite having no connected client.
        for (auth, marked, expected) in [
            (false, true, StatusCode::UNAUTHORIZED),
            (true, false, StatusCode::FORBIDDEN),
            (true, true, StatusCode::ACCEPTED),
        ] {
            let mut req = request("/api/whatsapp/unlink", auth.then_some("test-password-long"));
            *req.method_mut() = axum::http::Method::POST;
            if marked {
                req.headers_mut()
                    .insert("X-Carbon-Request", "1".parse().unwrap());
            }
            assert_eq!(
                router.clone().oneshot(req).await.unwrap().status(),
                expected
            );
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while rt.unlinking.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!rt.relink_required());
        assert!(rt.store.setting("whatsapp_logout").unwrap().is_none());
        assert!(rt.store.setting("whatsapp_session").unwrap().is_some());
        assert_eq!(rt.status()["whatsapp"], "Not linked");
        assert_eq!(
            rt.store.setting("model").unwrap().as_deref(),
            Some("preserved-model")
        );
    }
    #[tokio::test]
    async fn pairing_guards_and_code_lifecycle() {
        assert_eq!(
            test_app()
                .oneshot(pairing_request("12025550123", false, true))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            test_app()
                .oneshot(pairing_request("12025550123", true, false))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime::Runtime::open(dir.path(), 50, 500).unwrap();
        let router = app(Arc::new(AppState {
            credential_digest: Some(Sha256::digest(b"admin:test-password-long").into()),
            failures: Mutex::new(HashMap::new()),
            runtime: Some(rt.clone()),
        }));
        assert_eq!(
            router
                .clone()
                .oneshot(pairing_request("invalid", true, true))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            router
                .clone()
                .oneshot(pairing_request("12025550123", true, true))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        rt.pairing.lock().await.active = Some((
            "12025550123".into(),
            "TESTCODE".into(),
            Instant::now() + Duration::from_secs(90),
        ));
        let response = router
            .clone()
            .oneshot(pairing_request("+1 (202) 555-0123", true, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(data["code"], "TESTCODE");
        assert!(data["expires_in"].as_u64().unwrap() <= 90);
        assert_eq!(
            router
                .clone()
                .oneshot(pairing_request("12025550124", true, true))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let guard = rt.pairing.lock().await;
        assert_eq!(
            router
                .clone()
                .oneshot(pairing_request("12025550123", true, true))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(guard);
        {
            let mut state = rt.pairing.lock().await;
            state.active.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(1);
            state.retry_after = Some(Instant::now() + Duration::from_secs(60));
        }
        assert_eq!(
            router
                .oneshot(pairing_request("12025550123", true, true))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(rt.pairing.lock().await.active.is_none());
    }

    #[tokio::test]
    async fn optional_password_matches_camadmiral() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        let password = dir.path().join("password");
        std::fs::write(&config,serde_json::to_vec(&json!({"listen":"0.0.0.0:18080","auth":{"mode":"optional_basic","password_file":password}})).unwrap()).unwrap();
        let (_, state) = load_config(config.to_str().unwrap()).unwrap();
        let router = app(Arc::new(state));
        assert_eq!(
            router
                .clone()
                .oneshot(request("/api/status", None))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let mut mutation = request("/api/admins/bootstrap", None);
        *mutation.method_mut() = axum::http::Method::POST;
        assert_eq!(
            router.oneshot(mutation).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        std::fs::write(&password, "test-password-long\n").unwrap();
        let (_, state) = load_config(config.to_str().unwrap()).unwrap();
        let router = app(Arc::new(state));
        assert_eq!(
            router
                .clone()
                .oneshot(request("/api/status", None))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            router
                .oneshot(request("/api/status", Some("test-password-long")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        std::fs::write(&password, " \n").unwrap();
        assert!(load_config(config.to_str().unwrap()).is_err());
        std::fs::remove_file(&password).unwrap();
        std::fs::create_dir(&password).unwrap();
        assert!(load_config(config.to_str().unwrap()).is_err());
    }
}

#[cfg(test)]
#[path = "../tests/app/whatsapp_ingress_e2e.rs"]
mod whatsapp_ingress_e2e;

#[cfg(test)]
#[path = "../tests/app/ac_journey.rs"]
mod ac_journey;

#[cfg(test)]
#[path = "../tests/app/concurrency_e2e.rs"]
mod concurrency_e2e;

#[cfg(test)]
#[test]
fn existing_feature_configuration_is_accepted_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let bytes = br#"{
        "listen": "127.0.0.1:18080",
        "auth": {"mode": "disabled"},
        "experimental_tasks": true,
        "debug_trace": true
    }"#;
    std::fs::write(&path, bytes).unwrap();
    let (config, _) = load_config(path.to_str().unwrap()).unwrap();
    assert!(config.experimental_tasks);
    assert!(config.debug_trace);
    assert_eq!(config.model_concurrency, 4);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // Supporting existing options does not turn unknown fields into silent no-ops.
    let mut invalid: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    invalid["unknown_setting"] = true.into();
    std::fs::write(&path, invalid.to_string()).unwrap();
    assert!(load_config(path.to_str().unwrap()).is_err());
}

#[cfg(test)]
#[test]
fn model_concurrency_config_defaults_and_rejects_out_of_range() {
    let value = serde_json::json!({"listen":"127.0.0.1:18080","auth":{"mode":"disabled"}});
    let config: Config = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(config.model_concurrency, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    for limit in [0, 17] {
        let mut value = value.clone();
        value["model_concurrency"] = limit.into();
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(load_config(path.to_str().unwrap()).is_err());
    }
    for limit in [1, 4, 16] {
        let mut value = value.clone();
        value["model_concurrency"] = limit.into();
        std::fs::write(&path, value.to_string()).unwrap();
        assert_eq!(
            load_config(path.to_str().unwrap())
                .unwrap()
                .0
                .model_concurrency,
            limit as usize
        );
    }
}

#[cfg(test)]
#[path = "../tests/app/contact_consent_e2e.rs"]
mod contact_consent_e2e;

#[cfg(test)]
#[path = "../tests/app/inquiry_routing_e2e.rs"]
mod inquiry_routing_e2e;

#[cfg(test)]
#[path = "../tests/app/private_document_context_e2e.rs"]
mod private_document_context_e2e;
