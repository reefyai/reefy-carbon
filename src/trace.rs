//! Opt-in operator traces. Never expose this directory through resource tools.
use crate::{llm::Llm, store::Store};
use anyhow::{bail, Result};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

static WRITER: Mutex<()> = Mutex::new(());
const MAX_BYTES: usize = 8 * 1024 * 1024;
const TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const RETENTION: u64 = 7 * 86400;
impl Store {
    pub fn trace_enabled(&self) -> bool {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM settings WHERE key='debug_trace'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    }
    pub fn trace_export(&self, operation: &str) -> Result<Value> {
        Uuid::parse_str(operation)?;
        let _guard = WRITER.lock().unwrap();
        let dir = self.root.join("state/traces");
        let mut entries = Vec::new();
        let mut paths = Vec::new();
        if dir.exists() {
            for item in fs::read_dir(dir)? {
                let path = item?.path();
                if path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
                    s.starts_with(&format!("{operation}-")) && s.ends_with(".json")
                }) {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        let mut bytes = 0;
        let mut truncated = false;
        for path in paths {
            let raw = fs::read(path)?;
            bytes += raw.len();
            if bytes > 16 * 1024 * 1024 {
                truncated = true;
                break;
            }
            entries.push(serde_json::from_slice::<Value>(&raw)?);
        }
        Ok(
            json!({"enabled":self.trace_enabled(),"operation_id":operation,"missing":entries.is_empty(),"export_truncated":truncated,"retention_days":7,"retention_may_have_removed_entries":true,"entries":entries}),
        )
    }
    pub fn trace(&self, inbox: i64, kind: &str, data: Value) {
        if !self.trace_enabled() {
            return;
        }
        let result = (|| -> Result<()> {
            let _guard = WRITER.lock().unwrap();
            let operation: String = {
                let db = self.db.lock().unwrap();
                db.execute(
                    "INSERT OR IGNORE INTO inbox_operations VALUES(?,?)",
                    rusqlite::params![inbox, Uuid::new_v4().to_string()],
                )?;
                db.query_row(
                    "SELECT operation_id FROM inbox_operations WHERE inbox_id=?",
                    [inbox],
                    |r| r.get(0),
                )?
            };
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let dir = self.root.join("state/traces");
            fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
            }
            let mut entry = json!({"format_version":1,"build":include_str!("../VERSION").trim(),"operation_id":operation,"inbox":inbox,"created":now,"kind":kind,"capture":"complete","data":data});
            let mut bytes = serde_json::to_vec(&entry)?;
            if bytes.len() > MAX_BYTES {
                entry["data"] = json!({"original_bytes":bytes.len()});
                entry["capture"] = json!("omitted_size_limit");
                bytes = serde_json::to_vec(&entry)?;
            }
            let mut files = Vec::new();
            for item in fs::read_dir(&dir)? {
                let item = item?;
                if item.path().extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let meta = item.metadata()?;
                let time = meta.modified()?.duration_since(UNIX_EPOCH)?.as_secs();
                if now.saturating_sub(time) > RETENTION {
                    fs::remove_file(item.path())?;
                } else {
                    files.push((time, item.path(), meta.len()));
                }
            }
            files.sort_by_key(|f| f.0);
            let mut total: u64 = files.iter().map(|f| f.2).sum();
            let mut count = files.len();
            for (_, path, size) in files {
                if total + bytes.len() as u64 <= TOTAL_BYTES && count < 1000 {
                    break;
                }
                fs::remove_file(path)?;
                total -= size;
                count -= 1;
            }
            let path = dir.join(format!(
                "{operation}-{}-{}.json",
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
                Uuid::new_v4()
            ));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(&bytes)?;
            Ok(())
        })();
        if result.is_err() {
            self.log_work(
                inbox,
                "warn",
                "trace_write_failed",
                None,
                "Debug trace could not be saved",
            );
        }
    }
}
impl Llm {
    pub(crate) async fn traced_json(
        &self,
        store: &Store,
        inbox: i64,
        endpoint: &str,
        body: &Value,
    ) -> Result<Value> {
        self.traced_request(
            store,
            inbox,
            endpoint,
            body,
            self.client
                .post(format!("{}{endpoint}", self.base))
                .bearer_auth(&self.key)
                .json(body),
        )
        .await
    }
    pub(crate) async fn traced_request(
        &self,
        store: &Store,
        inbox: i64,
        endpoint: &str,
        body: &Value,
        request: reqwest::RequestBuilder,
    ) -> Result<Value> {
        let Some(template) = request.try_clone() else {
            return self
                .traced_attempt(store, inbox, endpoint, body, request)
                .await;
        };
        let mut result = self
            .traced_attempt(store, inbox, endpoint, body, request)
            .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        for seconds in [2, 4, 8, 8] {
            let Some(busy) = result
                .as_ref()
                .err()
                .and_then(|e| e.downcast_ref::<crate::llm::ModelBusy>())
            else {
                return result;
            };
            let delay =
                std::time::Duration::from_secs(busy.retry_after.unwrap_or(seconds).max(seconds));
            if delay >= deadline.saturating_duration_since(tokio::time::Instant::now()) {
                return result;
            }
            store.log_work(
                inbox,
                "warn",
                "llm_retry",
                None,
                "AI service busy; retrying within 30-second budget",
            );
            store.trace(
                inbox,
                "model_retry",
                json!({"delay_seconds":delay.as_secs(),"reason":"service_busy"}),
            );
            tokio::time::sleep(delay).await;
            let next = template.try_clone().expect("cloneable request template");
            result = match tokio::time::timeout_at(
                deadline,
                self.traced_attempt(store, inbox, endpoint, body, next),
            )
            .await
            {
                Ok(value) => value,
                Err(_) => return Err(crate::llm::ModelBusy { retry_after: None }.into()),
            };
        }
        result
    }
    async fn traced_attempt(
        &self,
        store: &Store,
        inbox: i64,
        endpoint: &str,
        body: &Value,
        request: reqwest::RequestBuilder,
    ) -> Result<Value> {
        let call = Uuid::new_v4().to_string();
        store.trace(
            inbox,
            "model_request",
            json!({"call_id":call,"endpoint":endpoint,"body":body}),
        );
        let result = async {
            let mut response = request.send().await?;
            let status = response.status();
            let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(|v| v.parse::<u64>().unwrap_or(30));
            let mut bytes = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = response.chunk().await? {
                let room = (1024 * 1024usize).saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if chunk.len() > room { truncated = true; break; }
            }
            store.trace(inbox,"model_response",json!({"call_id":call,"status":status.as_u16(),"truncated":truncated,"body":String::from_utf8_lossy(&bytes)}));
            if truncated { bail!("Proxy response too large"); }
            if status.as_u16() == 429 || status.as_u16() == 503 { return Err(crate::llm::ModelBusy { retry_after }.into()); }
            if status.is_server_error() { return Err(crate::execution::TemporaryModel.into()); }
            if !status.is_success() { bail!("Proxy returned HTTP {status}"); }
            let value: Value = serde_json::from_slice(&bytes)?;
            let calls = &value["choices"][0]["message"]["tool_calls"];
            let disposition = if calls.is_null() || calls.as_array().is_some_and(|a|a.is_empty()) {"no_tool_requested"} else if calls.as_array().is_some() {"tools_requested"} else {"malformed_tool_calls"};
            store.trace(inbox,"model_disposition",json!({"call_id":call,"disposition":disposition}));
            if disposition == "malformed_tool_calls" { bail!("Malformed tool calls"); }
            Ok(value)
        }.await;
        if let Err(error) = &result {
            store.trace(
                inbox,
                "model_error",
                json!({"call_id":call,"error":format!("{error:#}")}),
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn busy_retries_recover_or_return_specific_failure_without_replaying_tools() {
        use axum::{routing::post, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ingest("retry@test", "retry", "hello").unwrap();
        let work = store.next().unwrap().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/recover",
                post({
                    let count = count.clone();
                    move || {
                        let count = count.clone();
                        async move {
                            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                                (axum::http::StatusCode::TOO_MANY_REQUESTS, "busy")
                            } else {
                                (
                                    axum::http::StatusCode::OK,
                                    r#"{"choices":[{"message":{"content":"hello"}}]}"#,
                                )
                            }
                        }
                    }
                }),
            )
            .route(
                "/busy",
                post(|| async {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        [("retry-after", "30")],
                        "busy",
                    )
                }),
            )
            .route(
                "/denied",
                post(|| async { (axum::http::StatusCode::UNAUTHORIZED, "denied") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = Llm::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "test".into(),
        )
        .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let value = llm
            .traced_json(&store, work.id, "/recover", &json!({}))
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(value["choices"][0]["message"]["content"], "hello");
        let error = llm
            .traced_json(&store, work.id, "/busy", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            crate::llm::failure_reply(&error),
            "The AI service is busy. Please try again shortly."
        );
        let error = llm
            .traced_json(&store, work.id, "/denied", &json!({}))
            .await
            .unwrap_err();
        assert!(!crate::llm::failure_reply(&error).contains("busy"));
        server.abort();
    }

    #[tokio::test]
    async fn persistent_capacity_is_bounded_to_five_attempts() {
        use axum::{routing::post, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ingest("busy@test", "busy", "hello").unwrap();
        let work = store.next().unwrap().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/busy",
            post({
                let count = count.clone();
                move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        (axum::http::StatusCode::TOO_MANY_REQUESTS, "busy")
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = Llm::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "test".into(),
        )
        .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let start = tokio::time::Instant::now();
        let error = llm
            .traced_json(&store, work.id, "/busy", &json!({}))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<crate::llm::ModelBusy>().is_some());
        assert_eq!(count.load(Ordering::SeqCst), 5);
        assert!(start.elapsed() < std::time::Duration::from_secs(31));
        server.abort();
    }

    #[tokio::test]
    async fn trace_captures_raw_errors_and_classifies_calls_without_credentials() {
        use axum::{response::IntoResponse, routing::post, Router};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ingest("trace@test", "trace-test", "hello").unwrap();
        let work = store.next().unwrap().unwrap();
        store.trace(work.id, "disabled", json!({"secret":"not written"}));
        assert!(!dir.path().join("state/traces").exists());
        store
            .db
            .lock()
            .unwrap()
            .execute("INSERT INTO settings VALUES('debug_trace','1')", [])
            .unwrap();
        let app=Router::new()
            .route("/direct",post(||async{axum::Json(json!({"choices":[{"message":{"content":"hello"},"finish_reason":"stop"}],"usage":{"total_tokens":12}}))}))
            .route("/malformed",post(||async{axum::Json(json!({"choices":[{"message":{"tool_calls":"invalid"}}]}))}))
            .route("/tools",post(||async{axum::Json(json!({"choices":[{"message":{"tool_calls":[{"id":"x","function":{"name":"unknown","arguments":"{}"}}]}}]}))}))
            .route("/failure",post(||async{(axum::http::StatusCode::BAD_GATEWAY,"provider failure body").into_response()}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = Llm::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "KEY_MUST_NOT_BE_CAPTURED".into(),
        )
        .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for path in ["/direct", "/tools", "/malformed", "/failure"] {
            let result=llm.traced_json(&store,work.id,path,&json!({"model":"test","messages":[{"role":"user","content":"full private text"}]})).await;
            assert_eq!(result.is_ok(), matches!(path, "/direct" | "/tools"));
        }
        store.trace(
            work.id,
            "tool_result",
            json!({"ok":false,"error":"unsupported tool"}),
        );
        let op: String = store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT operation_id FROM inbox_operations WHERE inbox_id=?",
                [work.id],
                |r| r.get(0),
            )
            .unwrap();
        let export = store.trace_export(&op).unwrap();
        let text = export.to_string();
        for expected in [
            "full private text",
            "provider failure body",
            "no_tool_requested",
            "tools_requested",
            "malformed_tool_calls",
            "unsupported tool",
            "total_tokens",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        assert!(!text.contains("KEY_MUST_NOT_BE_CAPTURED"));
        assert_eq!(export["missing"], false);
        assert!(store.trace_export("../escape").is_err());
        assert_eq!(
            store.trace_export(&Uuid::new_v4().to_string()).unwrap()["missing"],
            true
        );
        store.trace(work.id, "oversized", json!({"text":"x".repeat(MAX_BYTES)}));
        assert!(store
            .trace_export(&op)
            .unwrap()
            .to_string()
            .contains("omitted_size_limit"));
        server.abort();
    }
}
