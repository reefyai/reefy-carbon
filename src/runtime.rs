use crate::{llm::Llm, store::Store};
use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::types::message::MessageInfo;

/// Deliver independent recipients concurrently, preserving one in-flight send per participant.
/// Await the bounded batch before the next model turn; this is not concurrent model execution.
pub(crate) async fn deliver_task_batch<F, Fut>(store: Arc<Store>, send: F) -> Result<usize>
where
    F: Fn(String, String, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = bool> + Send + 'static,
{
    let mut jobs = tokio::task::JoinSet::new();
    let send = Arc::new(send);
    for _ in 0..3 {
        let Some((id, recipient, text, stanza)) = store.claim_task_send()? else {
            break;
        };
        let store = store.clone();
        let send = send.clone();
        jobs.spawn(async move {
            let success = store.check_task_send(id).is_ok()
                && tokio::time::timeout(
                    Duration::from_secs(45),
                    send(recipient.clone(), text, stanza),
                )
                .await
                .unwrap_or(false);
            store.task_sent(id, success)?;
            store.log(
                if success { "info" } else { "warn" },
                "task_message_delivery",
                Some(&recipient),
                if success {
                    "Task message sent"
                } else {
                    "Task delivery uncertain; not retried"
                },
            );
            Ok::<_, anyhow::Error>(())
        });
    }
    let count = jobs.len();
    while let Some(result) = jobs.join_next().await {
        result??;
    }
    Ok(count)
}

pub struct Runtime {
    pub(crate) transport_epoch: AtomicU64,
    pub(crate) progress_sequence: AtomicU64,
    root: PathBuf,
    viewers: Mutex<HashMap<uuid::Uuid, Instant>>,
    saved_session: AtomicBool,
    relink_required: AtomicBool,
    session_generation: AtomicU64,
    session_events: Mutex<()>,
    bot: tokio::sync::Mutex<Option<BotHandle>>,
    activity: tokio::sync::Mutex<()>,
    pub unlinking: AtomicBool,
    pub store: Arc<Store>,
    pub llm: Llm,
    pub status: Mutex<String>,
    pub qr: Mutex<Option<(String, Instant)>>,
    qr_retry: Mutex<Option<Instant>>,
    pub client: Mutex<Option<Arc<Client>>>,
    pub pairing_client: Mutex<Option<Arc<Client>>>,
    pub pairing: tokio::sync::Mutex<Pairing>,
}
#[derive(Default)]
pub struct Pairing {
    pub retry_after: Option<Instant>,
    pub active: Option<(String, String, Instant)>,
}

fn display_identity(identity: &str, phone: Option<&str>, lid: Option<&str>) -> String {
    match phone.filter(|p| (7..=15).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())) {
        Some(phone) => match lid {
            Some(lid) => format!("+{phone} ({lid})"),
            None => format!("+{phone}"),
        },
        None => format!("Phone unavailable ({identity})"),
    }
}

pub fn normalize_phone(input: &str) -> Option<String> {
    if input.len() > 40
        || !input
            .chars()
            .all(|c| c.is_ascii_digit() || "+ -()".contains(c))
    {
        return None;
    }
    let digits: String = input.chars().filter(char::is_ascii_digit).collect();
    (digits.len() >= 7 && digits.len() <= 15 && !digits.starts_with('0')).then_some(digits)
}

impl Runtime {
    /// Shared live intake boundary: tests use decoded synthetic WhatsApp stanzas.
    pub(crate) async fn receive_message(&self, info: &MessageInfo, message: &wa::Message) {
        let chat = match crate::ingress::admit(info, self.unlinking.load(Ordering::SeqCst)) {
            Ok(chat) => chat.to_string(),
            Err(reason) => {
                self.store
                    .log("debug", "whatsapp_message_ignored", None, reason);
                return;
            }
        };
        // No message body, raw stanza, encryption material or device-specific identity is logged.
        if info.source.sender != info.source.chat {
            self.store.log(
                "debug",
                "whatsapp_linked_device",
                Some(&chat),
                "Linked-device sender matched canonical chat; legacy comparison would reject",
            );
        } else {
            self.store.log(
                "debug",
                "whatsapp_direct_message",
                Some(&chat),
                "Direct sender matched canonical chat",
            );
        }
        let (quoted, control_allowed) = crate::media::reply_context(message);
        match crate::media::MediaRef::from_message(message) {
            Ok(Some(media)) => {
                let caption = message
                    .get_caption()
                    .or_else(|| message.text_content())
                    .unwrap_or("");
                let result = self.store.ingest_media_reply(
                    &chat,
                    &info.id,
                    caption,
                    &media,
                    control_allowed,
                    quoted.as_deref(),
                );
                let (level, event, message) = match result {
                    Ok(true) => (
                        "info",
                        "attachment_queued",
                        "Attachment queued for private storage",
                    ),
                    Ok(false) => (
                        "debug",
                        "attachment_duplicate",
                        "Duplicate attachment ignored",
                    ),
                    Err(_) => (
                        "warn",
                        "attachment_rejected",
                        "Attachment rejected by admission limits",
                    ),
                };
                self.store
                    .log_incoming(&chat, &info.id, level, event, Some(&chat), message);
                return;
            }
            Err(_) => {
                self.store.log_incoming(
                    &chat,
                    &info.id,
                    "warn",
                    "attachment_rejected",
                    Some(&chat),
                    "Attachment metadata invalid or over size limit",
                );
                return;
            }
            Ok(None) => {}
        }
        let Some(text) = message.text_content() else {
            let base = message.get_base_message();
            let (event, message) = if base.document_message.as_option().is_some() {
                (
                    "file_unsupported",
                    "File received; file ingestion is not supported yet",
                )
            } else if base.image_message.as_option().is_some()
                || base.video_message.as_option().is_some()
                || base.audio_message.as_option().is_some()
            {
                (
                    "media_unsupported",
                    "Media received; media ingestion is not supported yet",
                )
            } else {
                ("message_unsupported", "Ignored unsupported message type")
            };
            self.store
                .log_incoming(&chat, &info.id, "warn", event, Some(&chat), message);
            return;
        };
        self.identity_label(&chat).await;
        match self
            .store
            .ingest_reply(&chat, &info.id, text, control_allowed, quoted.as_deref())
        {
            Ok(true) => {
                if text.trim().eq_ignore_ascii_case("stop") {
                    let _ = self.store.handle_incoming_stop(&chat, &info.id);
                }
                self.identity_label(&chat).await;
                self.store.log_incoming(
                    &chat,
                    &info.id,
                    "info",
                    "message_received",
                    Some(&chat),
                    "Private text message queued",
                );
            }
            Ok(false) => self.store.log_incoming(
                &chat,
                &info.id,
                "debug",
                "message_duplicate",
                Some(&chat),
                "Duplicate message ignored",
            ),
            Err(_) => self.store.log_incoming(
                &chat,
                &info.id,
                "warn",
                "message_rejected",
                Some(&chat),
                "Message rejected by storage or request limits",
            ),
        }
    }

    pub async fn identity_label(&self, identity: &str) -> String {
        let Ok(jid) = identity.parse::<Jid>() else {
            return identity.to_owned();
        };
        let client = self.client.lock().unwrap().clone();
        let mapping = if let Some(client) = client {
            client.get_lid_pn_entry(&jid).await.ok().flatten()
        } else {
            None
        };
        let phone = if jid.is_pn() {
            Some(jid.user.to_string())
        } else {
            mapping
                .as_ref()
                .filter(|m| m.lid.as_ref() == jid.user.as_str())
                .map(|m| m.phone_number.to_string())
        };
        let lid = if jid.is_lid() {
            Some(identity.to_owned())
        } else {
            mapping.as_ref().map(|m| format!("{}@lid", m.lid))
        };
        let label = display_identity(identity, phone.as_deref(), lid.as_deref());
        let _ = self.store.cache_identity_label(identity, &label);
        label
    }
    pub async fn participants(&self) -> Result<Value> {
        let mut rows = self.store.participants()?;
        for row in rows.as_array_mut().unwrap() {
            let identity = row["identity"].as_str().unwrap_or_default().to_owned();
            row["label"] = json!(self.identity_label(&identity).await);
        }
        Ok(rows)
    }
    pub async fn approval_requests(&self) -> Result<Value> {
        let mut data = self.store.approval_requests()?;
        if let Some(rows) = data["requests"].as_array_mut() {
            for row in rows {
                if let Some(identity) = row["identity"].as_str() {
                    row["identity_label"] = json!(self.identity_label(identity).await);
                }
            }
        }
        Ok(data)
    }
    pub async fn activity_rows(&self) -> Result<Value> {
        let mut data = self.store.activity()?;
        let mut labels = HashMap::new();
        for event in data["events"].as_array_mut().unwrap() {
            if let Some(identity) = event["identity"].as_str().map(str::to_owned) {
                if !labels.contains_key(&identity) {
                    labels.insert(identity.clone(), self.identity_label(&identity).await);
                }
                event["identity_label"] = json!(labels[&identity]);
            }
        }
        Ok(data)
    }
    pub fn open(root: &Path, user_limit: i64, global_limit: i64) -> Result<Arc<Self>> {
        Self::open_with_llm(root, user_limit, global_limit, Llm::from_env()?)
    }
    pub(crate) fn open_with_llm(
        root: &Path,
        user_limit: i64,
        global_limit: i64,
        llm: Llm,
    ) -> Result<Arc<Self>> {
        let mut store = Store::open(root)?;
        store.daily_user_limit = user_limit;
        store.daily_global_limit = global_limit;
        let relink_required = store.setting("whatsapp_logout")?.is_some();
        Ok(Arc::new(Self {
            root: root.to_path_buf(),
            viewers: Mutex::new(HashMap::new()),
            saved_session: AtomicBool::new(false),
            relink_required: AtomicBool::new(relink_required),
            session_generation: AtomicU64::new(0),
            session_events: Mutex::new(()),
            transport_epoch: AtomicU64::new(0),
            progress_sequence: AtomicU64::new(0),
            bot: tokio::sync::Mutex::new(None),
            activity: tokio::sync::Mutex::new(()),
            unlinking: AtomicBool::new(false),
            store: Arc::new(store),
            llm,
            status: Mutex::new(
                if relink_required {
                    "Logged out"
                } else {
                    "Starting"
                }
                .into(),
            ),
            qr: Mutex::new(None),
            qr_retry: Mutex::new(None),
            client: Mutex::new(None),
            pairing_client: Mutex::new(None),
            pairing: tokio::sync::Mutex::new(Pairing::default()),
        }))
    }
    pub fn set_pairing_viewer(&self, id: uuid::Uuid, open: bool) -> Result<()> {
        let mut viewers = self.viewers.lock().unwrap();
        let now = Instant::now();
        viewers.retain(|_, expiry| *expiry > now);
        if open {
            anyhow::ensure!(
                viewers.contains_key(&id) || viewers.len() < 16,
                "Too many pairing viewers"
            );
            viewers.insert(id, now + Duration::from_secs(30));
        } else {
            viewers.remove(&id);
        }
        Ok(())
    }
    fn pairing_visible(&self) -> bool {
        let mut viewers = self.viewers.lock().unwrap();
        let now = Instant::now();
        viewers.retain(|_, expiry| *expiry > now);
        !viewers.is_empty()
    }
    async fn maintain_pairing(self: &Arc<Self>) -> Result<()> {
        if self.relink_required()
            || self.unlinking.load(Ordering::SeqCst)
            || self.client.lock().unwrap().is_some()
            || self.saved_session.load(Ordering::SeqCst)
        {
            return Ok(());
        }
        let Ok(_activity) = self.activity.try_lock() else {
            return Ok(());
        };
        let Ok(pairing) = self.pairing.try_lock() else {
            return Ok(());
        };
        if self.unlinking.load(Ordering::SeqCst) || self.client.lock().unwrap().is_some() {
            return Ok(());
        }
        if self
            .pairing_client
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.is_logged_in())
        {
            return Ok(());
        }
        let visible = self.pairing_visible();
        let phone_active = pairing
            .active
            .as_ref()
            .is_some_and(|(_, _, expiry)| *expiry > Instant::now());
        if !visible && !phone_active {
            self.shutdown().await;
            *self.pairing_client.lock().unwrap() = None;
            *self.qr.lock().unwrap() = None;
            *self.status.lock().unwrap() = "Not linked".into();
        } else if visible && self.bot.lock().await.is_none() {
            let now = Instant::now();
            if self
                .qr_retry
                .lock()
                .unwrap()
                .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(5))
            {
                return Ok(());
            }
            *self.qr_retry.lock().unwrap() = Some(now);
            drop(pairing);
            self.start().await?;
        }
        Ok(())
    }
    pub fn status(&self) -> Value {
        let qr = self.qr.lock().unwrap().clone();
        let now = Instant::now();
        let visible = self.pairing_visible();
        let live = qr.as_ref().filter(|(_, expiry)| visible && *expiry > now);
        json!({"whatsapp":if self.unlinking.load(Ordering::SeqCst) { "Unlinking…".to_string() } else { self.status.lock().unwrap().clone() },
            "relink_required":self.relink_required(),
            "logout_reason":self.store.setting("whatsapp_logout").ok().flatten(),
            "has_qr":live.is_some(),
            "qr_version":live.map(|(code, _)| format!("{:x}", Sha256::digest(code.as_bytes()))),
            "qr_expires_in":live.map(|(_, expiry)| (*expiry-now).as_secs()),
            "model":self.store.setting("model").ok().flatten(),"true_admins":self.store.setting("true_admins").ok().flatten().and_then(|s|serde_json::from_str::<Value>(&s).ok()),"capabilities":["private_conversation","private_text_notes", "attachment_storage", "image_analysis", "file_replies", "document_analysis", "audio_transcription", "resource_search", "pending_reviews", "folder_read_inheritance", "note_revisions", "approval_admin_management", "scoped_text_coordination", "scoped_resources", "scoped_attachments"]})
    }
    fn qr_needs_refresh(&self, now: Instant, pairing: &Pairing) -> bool {
        self.pairing_visible()
            && !self.unlinking.load(Ordering::SeqCst)
            && self.client.lock().unwrap().is_none()
            && matches!(
                self.status.lock().unwrap().as_str(),
                "Scan QR code" | "Refreshing QR code…"
            )
            && self.qr.lock().unwrap().as_ref().is_some_and(|(_, expiry)| {
                now.saturating_duration_since(*expiry) >= Duration::from_secs(5)
            })
            && !pairing
                .active
                .as_ref()
                .is_some_and(|(_, _, expiry)| *expiry > now)
            && !self
                .qr_retry
                .lock()
                .unwrap()
                .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(30))
    }
    async fn refresh_expired_qr(self: &Arc<Self>) -> Result<()> {
        // Serialize session changes with unlink and active messaging work.
        let Ok(_activity) = self.activity.try_lock() else {
            return Ok(());
        };
        let Ok(pairing) = self.pairing.try_lock() else {
            return Ok(());
        };
        let now = Instant::now();
        if !self.qr_needs_refresh(now, &pairing) {
            return Ok(());
        }
        // Login can complete before the on_connected callback is delivered.
        if self
            .pairing_client
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.is_logged_in())
        {
            return Ok(());
        }
        *self.qr_retry.lock().unwrap() = Some(now);
        self.store
            .log("info", "qr_renewal", None, "Renewing expired QR batch");
        *self.status.lock().unwrap() = "Refreshing QR code…".into();
        self.shutdown().await;
        *self.pairing_client.lock().unwrap() = None;
        drop(pairing);
        // Reuse the same protocol store; this is renewal, never logout or session deletion.
        self.start().await
    }
    fn session_dir(&self) -> Result<PathBuf> {
        Ok(match self.store.setting("whatsapp_session")? {
            Some(id) => self
                .root
                .join("state")
                .join(format!("whatsapp-{}", uuid::Uuid::parse_str(&id)?)),
            None => self.root.join("state/whatsapp"),
        })
    }
    pub async fn shutdown(&self) {
        self.advance_session_generation();
        self.transport_epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(bot) = self.bot.lock().await.take() {
            bot.shutdown().await;
        }
    }
    pub async fn unlink(self: &Arc<Self>) -> Result<()> {
        self.store.log(
            "info",
            "unlink_started",
            None,
            "Unlinking WhatsApp and cancelling queued work",
        );
        // Wait for an in-flight reply before cancelling the remaining queue.
        let _activity = self.activity.lock().await;
        // logout() itself emits LoggedOut. Retire the old callbacks before
        // rotating storage, so intentional unlink cannot revoke the new store.
        self.advance_session_generation();
        let old_dir = self.session_dir()?;
        let client = self.client.lock().unwrap().take();
        // Commit the fresh session before network work, so a crash cannot reuse old keys.
        self.store
            .reset_whatsapp_session(&uuid::Uuid::new_v4().to_string())?;
        self.saved_session.store(false, Ordering::SeqCst);
        if let Some(client) = client {
            if tokio::time::timeout(Duration::from_secs(45), client.logout())
                .await
                .is_err()
            {
                client.disconnect().await;
            }
        }
        self.shutdown().await;
        self.relink_required.store(false, Ordering::SeqCst);
        *self.pairing_client.lock().unwrap() = None;
        *self.qr.lock().unwrap() = None;
        *self.pairing.lock().await = Pairing::default();
        match std::fs::remove_dir_all(&old_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.store.log(
            "info",
            "unlink_complete",
            None,
            "Local session cleared; starting fresh pairing",
        );
        *self.status.lock().unwrap() = "Starting".into();
        self.start().await
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if self.relink_required() {
            *self.status.lock().unwrap() = "Logged out".into();
            return Ok(());
        }
        let dir = self.session_dir()?;
        std::fs::create_dir_all(&dir)?;
        let backend = SqliteStore::new(dir.join("session.sqlite").to_str().unwrap()).await?;
        let saved = backend
            .load_device_data_for_device(backend.device_id())
            .await?
            .is_some_and(|d| d.pn.is_some());
        self.saved_session.store(saved, Ordering::SeqCst);
        if !saved && !self.pairing_visible() {
            *self.status.lock().unwrap() = "Not linked".into();
            return Ok(());
        }
        self.store.log(
            "info",
            "whatsapp_start",
            None,
            "Starting WhatsApp transport",
        );
        let qr = self.clone();
        let connected = self.clone();
        let logout = self.clone();
        let incoming = self.clone();
        let transport = self.clone();
        let generation = self.advance_session_generation();
        let bot = Bot::builder()
            .with_device_props(
                whatsapp_rust::wacore::store::DevicePropsOverride::new().with_os("Carbon"),
            )
            .with_backend(backend)
            .with_http_client(crate::media::http_client())
            .on_event_for(
                &[
                    EventKind::Disconnected,
                    EventKind::ConnectFailure,
                    EventKind::StreamError,
                ],
                move |event, _| {
                    let rt = transport.clone();
                    async move {
                        let _events = rt.session_events.lock().unwrap();
                        if rt.session_generation.load(Ordering::SeqCst) != generation
                            || rt.relink_required()
                        {
                            return;
                        }
                        rt.transport_epoch.fetch_add(1, Ordering::SeqCst);
                        *rt.client.lock().unwrap() = None;
                        *rt.status.lock().unwrap() = "Disconnected".into();
                        match &*event {
                            Event::Disconnected(_) => rt.store.log(
                                "info",
                                "whatsapp_disconnected",
                                None,
                                "WhatsApp transport disconnected",
                            ),
                            Event::ConnectFailure(failure) => {
                                eprintln!(
                                    "{}",
                                    json!({"event":"whatsapp_connect_failure_reason",
                                    "reason":format!("{:?}", failure.reason)})
                                );
                                rt.store.log(
                                    "error",
                                    "whatsapp_connect_failed",
                                    None,
                                    "WhatsApp connection failed",
                                );
                            }
                            Event::StreamError(_) => rt.store.log(
                                "error",
                                "whatsapp_stream_error",
                                None,
                                "WhatsApp transport error",
                            ),
                            _ => {}
                        }
                    }
                },
            )
            .on_qr_code(move |code: String, timeout: Duration| {
                let rt = qr.clone();
                async move {
                    let _events = rt.session_events.lock().unwrap();
                    if rt.session_generation.load(Ordering::SeqCst) != generation
                        || rt.relink_required()
                    {
                        return;
                    }
                    rt.saved_session.store(false, Ordering::SeqCst);
                    *rt.qr.lock().unwrap() = Some((code, Instant::now() + timeout));
                    *rt.status.lock().unwrap() = "Scan QR code".into();
                    rt.store
                        .log("debug", "qr_ready", None, "New QR code available");
                }
            })
            .on_connected(move |client: Arc<Client>| {
                let rt = connected.clone();
                async move {
                    {
                        let _events = rt.session_events.lock().unwrap();
                        if rt.session_generation.load(Ordering::SeqCst) != generation
                            || rt.relink_required()
                        {
                            return;
                        }
                        rt.transport_epoch.fetch_add(1, Ordering::SeqCst);
                        rt.saved_session.store(true, Ordering::SeqCst);
                        *rt.client.lock().unwrap() = Some(client);
                        *rt.qr.lock().unwrap() = None;
                        *rt.status.lock().unwrap() = "Connected".into();
                        rt.store
                            .log("info", "whatsapp_connected", None, "WhatsApp connected");
                    }
                    rt.pairing.lock().await.active = None;
                }
            })
            .on_logged_out(move |info| {
                let rt = logout.clone();
                async move {
                    rt.handle_logged_out(generation, &info).await;
                }
            })
            .on_message(move |ctx: MessageContext| {
                let rt = incoming.clone();
                async move {
                    rt.receive_message(&ctx.info, &ctx.message).await;
                }
            })
            .build()
            .await?;
        *self.pairing_client.lock().unwrap() = Some(bot.client());
        *self.bot.lock().await = Some(bot.spawn());
        Ok(())
    }
    pub fn relink_required(&self) -> bool {
        self.relink_required.load(Ordering::SeqCst)
    }

    fn advance_session_generation(&self) -> u64 {
        let _events = self.session_events.lock().unwrap();
        self.session_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn handle_logged_out(
        &self,
        generation: u64,
        info: &whatsapp_rust::wacore::types::events::LoggedOut,
    ) {
        {
            let _events = self.session_events.lock().unwrap();
            if self.session_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            // Retain only classified diagnostics. Raw stanzas can contain account
            // identifiers and one-time appeal tokens, so never log or expose them.
            let mut reason = format!("{:?}; on_connect={}", info.reason, info.on_connect);
            if let Some(raw) = &info.raw {
                // Only bounded, allowlisted protocol diagnostics, never the stanza.
                for key in ["code", "reason"] {
                    if let Some(code) = raw
                        .attrs()
                        .optional_string(key)
                        .and_then(|v| v.parse::<u16>().ok())
                    {
                        reason.push_str(&format!("; {key}={code}"));
                    }
                }
                if let Some(conflict) = raw.get_optional_child("conflict") {
                    let kind = conflict.attrs().optional_string("type");
                    let kind = match kind.as_deref() {
                        Some("device_removed") => "device_removed",
                        Some("replaced") => "replaced",
                        _ => "other",
                    };
                    reason.push_str(&format!("; conflict={kind}"));
                }
            }
            self.relink_required.store(true, Ordering::SeqCst);
            if self.store.record_whatsapp_logout(&reason).is_err() {
                self.store.log(
                    "error",
                    "logout_persistence_failed",
                    None,
                    "Could not persist WhatsApp logout; relinking still required",
                );
            }
            *self.client.lock().unwrap() = None;
            *self.qr.lock().unwrap() = None;
            self.saved_session.store(false, Ordering::SeqCst);
            self.transport_epoch.fetch_add(1, Ordering::SeqCst);
            *self.status.lock().unwrap() = "Logged out".into();
            self.store.log(
                "info",
                "whatsapp_logged_out",
                None,
                "WhatsApp logged out; explicit relinking required",
            );
            eprintln!(
                "{}",
                json!({"event":"whatsapp_logout_reason","reason":reason})
            );
        }
        self.pairing.lock().await.active = None;
    }
    pub async fn worker(self: Arc<Self>, limit: usize) {
        let mut lanes = tokio::task::JoinSet::new();
        lanes.spawn(self.clone().maintenance_loop());
        lanes.spawn(self.clone().scheduler_loop());
        lanes.spawn(self.clone().processing_loop(limit));
        lanes.spawn(self.clone().delivery_loop());
        if let Some(result) = lanes.join_next().await {
            self.store.log(
                "error",
                "worker_lane_stopped",
                None,
                if result.is_err() {
                    "Worker lane panicked; stopping remaining lanes"
                } else {
                    "Worker lane stopped unexpectedly"
                },
            );
        }
        // Dropping JoinSet aborts every lane and the processing pool's child tasks.
    }
    async fn maintenance_loop(self: Arc<Self>) {
        let mut cleanup_at = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.maintain_pairing().await.is_err() {
                self.store.log(
                    "error",
                    "pairing_state_failed",
                    None,
                    "Could not update pairing state",
                );
            }
            if self.refresh_expired_qr().await.is_err() {
                self.store.log(
                    "error",
                    "qr_renewal_failed",
                    None,
                    "QR renewal failed; retrying with backoff",
                );
            }
            if tokio::time::Instant::now() >= cleanup_at {
                let _ = self.llm.cleanup_files(&self.store).await;
                let _ = self.store.index_resources();
                cleanup_at = tokio::time::Instant::now() + Duration::from_secs(60);
            }
        }
    }
    pub(crate) async fn scheduler_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !self.unlinking.load(Ordering::SeqCst) && self.store.tasks_enabled() {
                let _ = self.store.tick_tasks(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64,
                );
            }
        }
    }
    async fn delivery_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _activity = self.activity.lock().await;
            if self.unlinking.load(Ordering::SeqCst) {
                continue;
            }
            let client = self.client.lock().unwrap().clone();
            let Some(client) = client else { continue };
            if let Ok(Some((id, recipient, text))) = self.store.claim_control() {
                let stanza = self.store.control_stanza(id);
                let success = match (recipient.parse::<Jid>(), stanza) {
                    (Ok(jid), Ok(stanza)) => tokio::time::timeout(
                        Duration::from_secs(45),
                        client.send_message_with_options(
                            jid,
                            wa::Message::text(&text),
                            whatsapp_rust::send::SendOptions::default().with_message_id(stanza),
                        ),
                    )
                    .await
                    .is_ok_and(|r| r.is_ok()),
                    _ => false,
                };
                let _ = self.store.control_sent(id, success);
                self.store.log(
                    if success { "info" } else { "warn" },
                    "review_notification",
                    Some(&recipient),
                    if success {
                        "Review notification sent"
                    } else {
                        "Review delivery uncertain; not retried automatically"
                    },
                );
                continue;
            }
            if self.store.tasks_enabled() {
                let sender = client.clone();
                match deliver_task_batch(self.store.clone(), move |recipient, text, stanza| {
                    let client = sender.clone();
                    async move {
                        match recipient.parse::<Jid>() {
                            Ok(jid) => client
                                .send_message_with_options(
                                    jid,
                                    wa::Message::text(&text),
                                    whatsapp_rust::send::SendOptions::default()
                                        .with_message_id(stanza),
                                )
                                .await
                                .is_ok(),
                            Err(_) => false,
                        }
                    }
                })
                .await
                {
                    Ok(0) => {}
                    Ok(_) => continue,
                    Err(_) => {
                        self.store.log(
                            "error",
                            "task_delivery_failed",
                            None,
                            "Task delivery state failed; uncertain sends are not replayed",
                        );
                        continue;
                    }
                }
            }

            if let Ok(Some((id, recipient, text))) = self.store.claim_send() {
                self.store.log_outbox(
                    id,
                    "info",
                    "reply_sending",
                    Some(&recipient),
                    "Sending reply",
                );
                let success = match (
                    recipient.parse::<Jid>(),
                    self.store.outbound_reply_stanza(id),
                ) {
                    (Ok(jid), Ok(stanza)) => tokio::time::timeout(
                        Duration::from_secs(45),
                        client.send_message_with_options(
                            jid,
                            wa::Message::text(&text),
                            whatsapp_rust::send::SendOptions::default().with_message_id(stanza),
                        ),
                    )
                    .await
                    .is_ok_and(|r| r.is_ok()),
                    _ => false,
                };
                if self.store.sent(id, success).is_err() {
                    self.store.log_outbox(
                        id,
                        "error",
                        "reply_state_failed",
                        Some(&recipient),
                        "Cannot persist outbound result",
                    );
                }
                if success {
                    self.store
                        .log_outbox(id, "info", "reply_sent", Some(&recipient), "Reply sent");
                } else {
                    self.store.log_outbox(
                        id,
                        "warn",
                        "reply_unknown",
                        Some(&recipient),
                        "Send failed or timed out; delivery uncertain",
                    );
                }
                continue;
            }
            if let Ok(Some(item)) = self.store.claim_file() {
                self.store.log_work(
                    item.work.id,
                    "info",
                    "file_sending",
                    Some(&item.work.recipient),
                    "Uploading authorized file reply",
                );
                let state = crate::delivery::deliver(&self.store, &client, &item)
                    .await
                    .unwrap_or("failed");
                if self.store.file_delivery_result(item.id, state).is_err() {
                    self.store.log_work(
                        item.work.id,
                        "error",
                        "file_state_failed",
                        Some(&item.work.recipient),
                        "Cannot persist file delivery result",
                    );
                }
                let (event, message) = match state {
                    "sent" => ("file_sent", "File reply sent"),
                    "unknown" => (
                        "file_unknown",
                        "File delivery uncertain; not retried automatically",
                    ),
                    "cancelled" => (
                        "file_cancelled",
                        "File delivery cancelled after access changed",
                    ),
                    _ => ("file_failed", "File retrieval or upload failed; not sent"),
                };
                self.store.log_work(
                    item.work.id,
                    if state == "sent" { "info" } else { "warn" },
                    event,
                    Some(&item.work.recipient),
                    message,
                );
                continue;
            }
            if let Ok(Some(item)) = self.store.claim_conversation_file() {
                self.store.log_work(
                    item.inbox,
                    "info",
                    "conversation_file_sending",
                    Some(&item.recipient),
                    "Uploading an authorized pair attachment",
                );
                let state = crate::scoped_resources::deliver(&self.store, &client, &item)
                    .await
                    .unwrap_or("failed");
                let persisted = self.store.conversation_file_result(item.id, state).is_ok();
                self.store.log_work(
                    item.inbox,
                    if state == "sent" && persisted {
                        "info"
                    } else {
                        "warn"
                    },
                    "conversation_file_delivery",
                    Some(&item.recipient),
                    match (state, persisted) {
                        ("sent", true) => "Pair attachment sent",
                        ("unknown", true) => {
                            "Attachment delivery uncertain; not retried automatically"
                        }
                        _ => "Pair attachment failed or was cancelled",
                    },
                );
                continue;
            }
        }
    }
    async fn processing_loop(self: Arc<Self>, limit: usize) {
        let mut pool = crate::concurrency::ProcessingPool::new(self.store.clone(), limit);
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pool.reap();
            if self.unlinking.load(Ordering::SeqCst) {
                continue;
            }
            let client = self.client.lock().unwrap().clone();
            let Some(client) = client else { continue };
            let model = self.store.setting("model").ok().flatten();
            let rt = self.clone();
            if pool
                .fill(model.is_some(), move |job| {
                    let rt = rt.clone();
                    let client = client.clone();
                    let model = model.clone();
                    async move { rt.process_job(client, job, model).await }
                })
                .is_err()
            {
                self.store.log(
                    "error",
                    "processing_dispatch_failed",
                    None,
                    "Could not claim processing work",
                );
            }
        }
    }
    async fn process_job(
        self: Arc<Self>,
        client: Arc<Client>,
        job: crate::concurrency::Job,
        model: Option<String>,
    ) {
        match job {
            crate::concurrency::Job::Pair(task) => {
                let Some(model) = model else {
                    let _ = self.store.conversation_result(task.id, "failed");
                    return;
                };
                self.store.log_work(
                    task.inbox,
                    "info",
                    "conversation_drafting",
                    Some(&task.recipient),
                    "Drafting from isolated pair context",
                );
                let feedback = crate::progress::WhatsApp::new(
                    self.clone(),
                    client.clone(),
                    crate::progress::Target::pair(&task),
                );
                let result = crate::progress::during(
                    feedback,
                    self.llm.conversation_draft(&self.store, &task, &model),
                )
                .await;
                // Only network delivery takes the unlink barrier; model waits never hold it.
                let _activity = self.activity.lock().await;
                if self.unlinking.load(Ordering::SeqCst) || self.client.lock().unwrap().is_none() {
                    let _ = self.store.conversation_result(task.id, "cancelled");
                    return;
                }
                let state = match result
                    .and_then(|draft| self.store.prepare_conversation_send(&task, &draft))
                {
                    Ok(text) => match task.recipient.parse::<Jid>() {
                        Ok(jid) => {
                            if tokio::time::timeout(
                                Duration::from_secs(45),
                                client.send_message_with_options(
                                    jid,
                                    wa::Message::text(&text),
                                    whatsapp_rust::send::SendOptions::default()
                                        .with_message_id(task.stanza.clone()),
                                ),
                            )
                            .await
                            .is_ok_and(|r| r.is_ok())
                            {
                                "sent"
                            } else {
                                "unknown"
                            }
                        }
                        Err(_) => "failed",
                    },
                    Err(_) => "failed",
                };
                let persisted = self.store.conversation_result(task.id, state).is_ok();
                self.store.log_work(
                    task.inbox,
                    if state == "sent" && persisted {
                        "info"
                    } else {
                        "warn"
                    },
                    "conversation_delivery",
                    Some(&task.recipient),
                    match (state, persisted) {
                        ("sent", true) => "Conversation message sent",
                        ("unknown", true) => "Conversation delivery uncertain; not retried",
                        _ => "Conversation delivery failed or state could not be persisted",
                    },
                );
            }
            crate::concurrency::Job::Private(work) => {
                let feedback = crate::progress::WhatsApp::new(
                    self.clone(),
                    client.clone(),
                    crate::progress::Target::Private(work.clone()),
                );
                if crate::progress::during(feedback, self.process_work(&client, &work, model))
                    .await
                    .is_err()
                {
                    let _ = self.store.cancel_task_actions(&work);
                    let _ = self.store.cancel_staged_files(&work);
                    let _ = self.store.cancel_staged_conversations(&work);
                    if !self.unlinking.load(Ordering::SeqCst)
                        && self.client.lock().unwrap().is_some()
                    {
                        let _ = self.store.finish(
                            &work,
                            "I couldn't complete this request. Please try again later.",
                        );
                    }
                    let _ = self.store.db.lock().unwrap().execute(
                        "UPDATE inbox SET state='interrupted' WHERE id=? AND state='processing'",
                        [work.id],
                    );
                    self.store.log_work(work.id,"warn","processing_cancelled",Some(&work.recipient),"Processing stopped after cancellation, disconnect, access change or deadline");
                }
            }
        }
    }
    async fn process_work(
        &self,
        client: &Client,
        work: &crate::store::Work,
        model: Option<String>,
    ) -> Result<()> {
        match self.store.pending_media(work) {
            Ok(Some(media)) => {
                self.store.log_work(
                    work.id,
                    "info",
                    "attachment_downloading",
                    Some(&work.recipient),
                    "Downloading attachment",
                );
                let downloaded = async {
                    let params = media.download_params()?;
                    let writer = client
                        .download_to_writer(&params, crate::media::BoundedWriter::default())
                        .await?;
                    self.store
                        .save_attachment(work, &media, &writer.0.into_inner())
                };
                match tokio::time::timeout(Duration::from_secs(45), downloaded).await {
                    Ok(Ok(_)) => {
                        self.store.log_work(
                            work.id,
                            "info",
                            "attachment_saved",
                            Some(&work.recipient),
                            "Attachment stored privately in Git/LFS",
                        );
                    }
                    _ => {
                        let _ = self.store.fail_media(work);
                        let _=self.store.finish(work,"I couldn't store this attachment. Please resend it; the limit is 8 MiB per file.");
                        self.store.log_work(
                            work.id,
                            "warn",
                            "attachment_failed",
                            Some(&work.recipient),
                            "Attachment download or storage failed",
                        );
                        return Ok(());
                    }
                }
            }
            Ok(None) => {}
            Err(_) => {
                let _ = self.store.fail_media(work);
                let _ = self.store.finish(
                    work,
                    "I couldn't read the attachment request. Please resend it.",
                );
                return Ok(());
            }
        }
        self.process_message(work, model).await
    }

    /// A channel can be revoked between model/transport selection and turn publication.
    pub(crate) fn finish_processed_turn(&self, work: &crate::store::Work, text: &str) {
        if self.store.finish(work, text).is_ok() {
            return;
        }
        self.store.log_work(
            work.id,
            "warn",
            "turn_publication_cancelled",
            Some(&work.recipient),
            "Turn publication failed; staged actions cancelled",
        );
        let cancelled = self.store.cancel_task_actions(work).is_ok()
            && self.store.cancel_staged_files(work).is_ok()
            && self.store.cancel_staged_conversations(work).is_ok();
        if !cancelled || self.store.finish(work,"I couldn't route or complete this request. Your message wasn't forwarded. Please try again.").is_err() {
            crate::concurrency::Job::Private(work.clone()).abandon(&self.store);
        }
    }

    /// Shared production processing after transport-specific attachment download.
    /// Text E2E uses this exact path, replacing only WhatsApp ingress/delivery.
    pub(crate) async fn process_message(
        &self,
        work: &crate::store::Work,
        model: Option<String>,
    ) -> Result<()> {
        if self.store.tasks_enabled() && self.store.stop_task_channels(work)? {
            self.store
                .finish(work, "Stopped incoming task conversations.")?;
            return Ok(());
        }
        if self.store.tasks_enabled() {
            match self.store.route_task_quote(work)? {
                crate::inquiry_routing::QuoteRoute::Attached => {
                    self.finish_processed_turn(work, "[WAIT]");
                    self.store.log_work(
                        work.id,
                        "info",
                        "inquiry_reply_routed",
                        Some(&work.recipient),
                        "Quoted reply associated with its authenticated inquiry",
                    );
                    return Ok(());
                }
                crate::inquiry_routing::QuoteRoute::Unavailable => {
                    self.store.finish(
                        work,
                        "This inquiry reply is unavailable. Your message wasn't forwarded.",
                    )?;
                    return Ok(());
                }
                crate::inquiry_routing::QuoteRoute::Private => {}
            }
            if self.store.clarification_quote_unavailable(work)? {
                self.store.finish(
                    work,
                    "This clarification is no longer available. Your message wasn't forwarded.",
                )?;
                return Ok(());
            }
        }
        match self.store.route_conversation(work) {
            Ok(crate::coordination::Route::Private) => {}
            Ok(crate::coordination::Route::Queued) => {
                self.store.log_work(
                    work.id,
                    "info",
                    "conversation_queued",
                    Some(&work.recipient),
                    "Quoted reply queued within its pair context",
                );
                return Ok(());
            }
            Ok(crate::coordination::Route::Reply(reply)) => {
                let _ = self.store.finish(work, reply);
                return Ok(());
            }
            Err(_) => {
                let _ = self.store.finish(
                    work,
                    "I couldn't route this conversation reply. Please try again later.",
                );
                return Ok(());
            }
        }
        if let Some(reply) = self.store.handle_contact_control(work) {
            self.store.finish(work, &reply)?;
            self.store.log_work(
                work.id,
                "info",
                "contact_consent_decided",
                Some(&work.recipient),
                "Participant contact consent handled by Rust",
            );
            return Ok(());
        }
        if self.store.admin_control_bound(work)? {
            if let Some(reply) = self.store.handle_control(work) {
                if self.store.finish(work, &reply).is_ok() {
                    self.store.log_work(
                        work.id,
                        "info",
                        "review_command",
                        Some(&work.recipient),
                        "Request-bound review command handled by Rust",
                    );
                }
                return Ok(());
            }
        }
        let Some(model) = model else {
            self.store.private_model_context(work)?;
            let _=self.store.finish(work,"Your attachment was saved privately. Select a language model in Carbon to discuss or organize it.");
            return Ok(());
        };
        self.store.log_work(
            work.id,
            "info",
            "llm_started",
            Some(&work.recipient),
            "Generating reply",
        );
        let text = match self.llm.respond(self.store.clone(), work, &model).await {
            Ok(t) => {
                self.store.log_work(
                    work.id,
                    "info",
                    "llm_complete",
                    Some(&work.recipient),
                    "Reply generated",
                );
                t
            }
            Err(error) => {
                if crate::execution::temporary(&error) && self.store.defer_execution(work)? {
                    self.store.log_work(
                        work.id,
                        "warn",
                        "execution_deferred",
                        Some(&work.recipient),
                        "Temporary model failure; saved turn will resume automatically",
                    );
                    return Ok(());
                }
                if self.store.cancel_task_actions(work).is_err()
                    || self.store.cancel_staged_files(work).is_err()
                    || self.store.cancel_staged_conversations(work).is_err()
                {
                    self.store.log_work(
                        work.id,
                        "error",
                        "file_cancel_failed",
                        Some(&work.recipient),
                        "Cannot cancel staged files; leaving work interrupted",
                    );
                    return Ok(());
                }
                self.store.log_work(
                    work.id,
                    "error",
                    "llm_failed",
                    Some(&work.recipient),
                    "Model request failed; sending fallback reply",
                );
                crate::llm::failure_reply(&error).into()
            }
        };
        self.finish_processed_turn(work, &text);
        Ok(())
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use whatsapp_rust::wacore::types::events::{ConnectFailureReason, LoggedOut};

    #[tokio::test]
    async fn revoked_saved_session_stays_terminal_until_explicit_relink() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open(dir.path(), 50, 500).unwrap();
        let session = rt.session_dir().unwrap();
        std::fs::create_dir_all(&session).unwrap();
        // Real protocol storage with a saved companion identity: the old startup
        // logic mistakes this for a usable session even after server logout.
        let backend = SqliteStore::new(session.join("session.sqlite").to_str().unwrap())
            .await
            .unwrap();
        let mut device = whatsapp_rust::wacore::store::Device::new();
        device.pn = Some("15555550123:1@s.whatsapp.net".parse().unwrap());
        backend
            .save_device_data_for_device(backend.device_id(), &device)
            .await
            .unwrap();
        drop(backend);
        rt.store.set_model("keep-model").unwrap();
        let event = LoggedOut::builder()
            .on_connect(false)
            .reason(ConnectFailureReason::LoggedOut)
            .raw(
                whatsapp_rust::NodeBuilder::new("stream:error")
                    .attr("code", "401")
                    .attr("appeal_token", "secret-appeal-token")
                    .children([whatsapp_rust::NodeBuilder::new("conflict")
                        .attr("type", "device_removed")
                        .build()])
                    .build(),
            )
            .build();
        rt.handle_logged_out(0, &event).await;
        rt.set_pairing_viewer(uuid::Uuid::new_v4(), true).unwrap();
        rt.start().await.unwrap();
        for _ in 0..3 {
            rt.maintain_pairing().await.unwrap();
        }
        assert!(
            rt.bot.lock().await.is_none(),
            "never retry revoked saved credentials"
        );
        assert_eq!(rt.status()["whatsapp"], "Logged out");
        assert_eq!(rt.status()["relink_required"], true);
        assert_eq!(rt.status()["has_qr"], false);
        assert_eq!(
            rt.status()["logout_reason"],
            "LoggedOut; on_connect=false; code=401; conflict=device_removed"
        );
        assert!(!rt.status().to_string().contains("secret-appeal-token"));
        assert!(!rt
            .store
            .activity()
            .unwrap()
            .to_string()
            .contains("secret-appeal-token"));
        assert!(
            session.join("session.sqlite").exists(),
            "logout alone must not erase keys"
        );
        drop(rt);

        let rt = Runtime::open(dir.path(), 50, 500).unwrap();
        rt.start().await.unwrap();
        assert!(
            rt.relink_required(),
            "restart must not lose terminal logout"
        );
        assert!(rt.bot.lock().await.is_none());
        // No visible pairing viewer: explicit reset exercises the real cleanup
        // and fresh protocol store without connecting a real WhatsApp account.
        rt.unlink().await.unwrap();
        assert!(!rt.relink_required());
        assert!(!session.exists());
        assert_ne!(rt.session_dir().unwrap(), session);
        assert!(rt.session_dir().unwrap().join("session.sqlite").exists());
        assert_eq!(rt.status()["whatsapp"], "Not linked");
        assert_eq!(
            rt.store.setting("model").unwrap().as_deref(),
            Some("keep-model")
        );
        // An event delivered by the replaced transport cannot revoke the new link.
        rt.handle_logged_out(0, &event).await;
        assert!(!rt.relink_required());
        assert!(rt.store.setting("whatsapp_logout").unwrap().is_none());
    }

    #[tokio::test]
    async fn logout_diagnostics_distinguish_account_lock_from_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open(dir.path(), 50, 500).unwrap();
        let event = LoggedOut::builder()
            .on_connect(true)
            .reason(ConnectFailureReason::AccountLocked)
            .build();
        rt.handle_logged_out(0, &event).await;
        assert_eq!(
            rt.status()["logout_reason"],
            "AccountLocked; on_connect=true"
        );
        rt.start().await.unwrap();
        assert!(
            rt.bot.lock().await.is_none(),
            "account locks must not be retried automatically"
        );
    }
    #[tokio::test]
    async fn pairing_stays_idle_without_visible_viewers() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open(dir.path(), 50, 500).unwrap();
        rt.start().await.unwrap();
        assert!(rt.bot.lock().await.is_none());
        assert_eq!(rt.status()["whatsapp"], "Not linked");
        assert_eq!(rt.status()["has_qr"], false);
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        rt.set_pairing_viewer(first, true).unwrap();
        rt.set_pairing_viewer(second, true).unwrap();
        rt.set_pairing_viewer(first, false).unwrap();
        assert!(rt.pairing_visible());
        rt.viewers
            .lock()
            .unwrap()
            .insert(second, Instant::now() - Duration::from_secs(1));
        assert!(!rt.pairing_visible());
        rt.maintain_pairing().await.unwrap();
        assert!(rt.bot.lock().await.is_none());
        for _ in 0..16 {
            rt.set_pairing_viewer(uuid::Uuid::new_v4(), true).unwrap();
        }
        assert!(rt.set_pairing_viewer(uuid::Uuid::new_v4(), true).is_err());
    }
    #[tokio::test]
    async fn qr_refresh_respects_expiry_pairing_and_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open(dir.path(), 50, 500).unwrap();
        let now = Instant::now();
        rt.set_pairing_viewer(uuid::Uuid::new_v4(), true).unwrap();
        let mut pairing = Pairing::default();
        *rt.status.lock().unwrap() = "Scan QR code".into();
        assert!(!rt.qr_needs_refresh(now, &pairing));
        *rt.qr.lock().unwrap() = Some(("test-qr".into(), now + Duration::from_secs(20)));
        let status = rt.status();
        assert_eq!(status["has_qr"], true);
        assert!(status["qr_expires_in"].as_u64().unwrap() <= 20);
        assert!(!status.to_string().contains("test-qr"));
        assert!(!rt.qr_needs_refresh(now, &pairing));
        *rt.qr.lock().unwrap() = Some(("test-qr".into(), now - Duration::from_secs(6)));
        assert_eq!(rt.status()["has_qr"], false);
        assert!(rt.qr_needs_refresh(now, &pairing));
        pairing.active = Some((
            "12025550123".into(),
            "TESTCODE".into(),
            now + Duration::from_secs(40),
        ));
        assert!(!rt.qr_needs_refresh(now, &pairing));
        pairing.active.as_mut().unwrap().2 = now - Duration::from_secs(1);
        assert!(rt.qr_needs_refresh(now, &pairing));
        *rt.qr_retry.lock().unwrap() = Some(now);
        assert!(!rt.qr_needs_refresh(now, &pairing));
        assert!(rt.qr_needs_refresh(now + Duration::from_secs(31), &pairing));
        rt.unlinking.store(true, Ordering::SeqCst);
        assert!(!rt.qr_needs_refresh(now + Duration::from_secs(31), &pairing));
        rt.unlinking.store(false, Ordering::SeqCst);
        *rt.status.lock().unwrap() = "Connected".into();
        assert!(!rt.qr_needs_refresh(now + Duration::from_secs(31), &pairing));
    }
    #[test]
    fn phone_labels_use_only_supplied_valid_phone_mappings() {
        assert_eq!(
            display_identity("123@lid", Some("15550000001"), Some("123@lid")),
            "+15550000001 (123@lid)"
        );
        assert_eq!(
            display_identity("123@lid", None, Some("123@lid")),
            "Phone unavailable (123@lid)"
        );
        assert_eq!(
            display_identity("123@lid", Some("not-a-phone"), None),
            "Phone unavailable (123@lid)"
        );
    }
    #[test]
    fn international_phone_validation() {
        assert_eq!(
            normalize_phone("+1 (202) 555-0123").as_deref(),
            Some("12025550123")
        );
        for value in [
            "",
            "0123456789",
            "123",
            "1234567890123456",
            "call123456789",
            "１２３４５６７",
            "1234567\n",
        ] {
            assert!(normalize_phone(value).is_none(), "{value:?}");
        }
    }
}
