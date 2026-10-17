//! Bounded, durable references to authenticated WhatsApp attachment downloads.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Seek, SeekFrom, Write};
use whatsapp_rust::{
    download::{DownloadParams, DownloadWriter, Downloadable, MediaType},
    prelude::*,
};

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub fn http_client() -> whatsapp_rust::http::UreqHttpClient {
    let agent = ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .max_redirects(0)
        .input_buffer_size(16384)
        .output_buffer_size(16384)
        .max_idle_connections(4)
        .max_idle_connections_per_host(2)
        .build()
        .into();
    whatsapp_rust::http::UreqHttpClient::with_agent(agent)
        .with_max_body_bytes(MAX_BYTES as u64 + 1024)
}
/// Only transport metadata selects a conversation; quoted bodies never act as authority.
pub(crate) fn reply_context(message: &wa::Message) -> (Option<String>, bool) {
    let m = message.get_base_message();
    let context = m
        .extended_text_message
        .as_option()
        .and_then(|v| v.context_info.as_option())
        .or_else(|| {
            m.image_message
                .as_option()
                .and_then(|v| v.context_info.as_option())
        })
        .or_else(|| {
            m.document_message
                .as_option()
                .and_then(|v| v.context_info.as_option())
        })
        .or_else(|| {
            m.audio_message
                .as_option()
                .and_then(|v| v.context_info.as_option())
        })
        .or_else(|| {
            m.video_message
                .as_option()
                .and_then(|v| v.context_info.as_option())
        });
    (
        context.and_then(|c| c.stanza_id.clone()),
        !context.is_some_and(|c| {
            c.is_forwarded.unwrap_or(false) || c.forwarding_score.unwrap_or(0) > 0
        }),
    )
}
pub(crate) fn image_mime(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\xff\xd8\xff") {
        Ok("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Ok("image/webp")
    } else {
        anyhow::bail!("Only JPEG, PNG and WebP images are supported")
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRef {
    pub kind: String,
    pub name: String,
    pub mime: String,
    pub path: String,
    pub key: Vec<u8>,
    pub sha: Vec<u8>,
    pub encrypted_sha: Vec<u8>,
    pub size: u64,
}
impl MediaRef {
    pub fn from_message(message: &wa::Message) -> Result<Option<Self>> {
        ensure!(
            !message.is_view_once()
                && !message.is_ephemeral()
                && message
                    .get_base_message()
                    .get_ephemeral_expiration()
                    .unwrap_or(0)
                    == 0,
            "Disappearing messages are not persisted"
        );
        let m = message.get_base_message();
        let parts: Option<(&dyn Downloadable, &str, &str, &str)> =
            if let Some(v) = m.image_message.as_option() {
                Some((
                    v,
                    "image",
                    "photo",
                    v.mimetype.as_deref().unwrap_or("application/octet-stream"),
                ))
            } else if let Some(v) = m.document_message.as_option() {
                Some((
                    v,
                    "document",
                    v.file_name.as_deref().unwrap_or("document"),
                    v.mimetype.as_deref().unwrap_or("application/octet-stream"),
                ))
            } else if let Some(v) = m.video_message.as_option() {
                Some((
                    v,
                    "video",
                    "video",
                    v.mimetype.as_deref().unwrap_or("application/octet-stream"),
                ))
            } else if let Some(v) = m.audio_message.as_option() {
                Some((
                    v,
                    "audio",
                    "audio",
                    v.mimetype.as_deref().unwrap_or("application/octet-stream"),
                ))
            } else {
                None
            };
        let Some((d, kind, name, mime)) = parts else {
            return Ok(None);
        };
        let value = Self {
            kind: kind.into(),
            name: name.chars().filter(|c| !c.is_control()).take(100).collect(),
            mime: mime.into(),
            path: d.direct_path().unwrap_or("").into(),
            key: d.media_key().unwrap_or_default().to_vec(),
            sha: d.file_sha256().unwrap_or_default().to_vec(),
            encrypted_sha: d.file_enc_sha256().unwrap_or_default().to_vec(),
            size: d.file_length().unwrap_or(0),
        };
        value.validate()?;
        Ok(Some(value))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.size > 0 && self.size <= MAX_BYTES as u64,
            "Attachment must be 1 byte to 8 MiB"
        );
        ensure!(
            self.key.len() == 32 && self.sha.len() == 32 && self.encrypted_sha.len() == 32,
            "Incomplete media integrity metadata"
        );
        ensure!(
            self.path.starts_with('/')
                && !self.path.starts_with("//")
                && !self.path.contains('\\')
                && !self.path.chars().any(char::is_control)
                && self.path.len() <= 4096,
            "Invalid media path"
        );
        ensure!(
            !self.name.is_empty()
                && self.name.len() <= 400
                && self.mime.len() <= 128
                && !self.mime.chars().any(char::is_control),
            "Invalid media metadata"
        );
        ensure!(
            ["image", "document", "video", "audio"].contains(&self.kind.as_str()),
            "Unsupported media kind"
        );
        Ok(())
    }
    pub fn download_params(&self) -> Result<DownloadParams> {
        self.validate()?;
        let kind = match self.kind.as_str() {
            "image" => MediaType::Image,
            "video" => MediaType::Video,
            "audio" => MediaType::Audio,
            _ => MediaType::Document,
        };
        Ok(DownloadParams::encrypted(
            &self.path,
            &self.key,
            &self.sha,
            &self.encrypted_sha,
            self.size,
            kind,
        ))
    }
}
#[derive(Default)]
pub struct BoundedWriter(pub Cursor<Vec<u8>>);
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.position().saturating_add(bytes.len() as u64) > MAX_BYTES as u64 {
            return Err(std::io::Error::other("Media size limit"));
        }
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Seek for BoundedWriter {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}
impl DownloadWriter for BoundedWriter {
    fn truncate(&mut self, len: u64) -> std::io::Result<()> {
        self.0
            .get_mut()
            .truncate(len.min(MAX_BYTES as u64) as usize);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use sha2::{Digest, Sha256};
    pub fn reference(bytes: &[u8]) -> MediaRef {
        MediaRef {
            kind: "document".into(),
            name: "../../untrusted.txt".into(),
            mime: "text/plain".into(),
            path: "/v/t62/media".into(),
            key: vec![1; 32],
            sha: Sha256::digest(bytes).to_vec(),
            encrypted_sha: vec![2; 32],
            size: bytes.len() as u64,
        }
    }
    #[test]
    fn media_http_client_does_not_follow_redirects() {
        use std::io::Read;
        use whatsapp_rust::http::{HttpClient, HttpRequest};
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let redirect = target.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            write!(stream,"HTTP/1.1 302 Found\r\nLocation: http://{redirect}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let response = http_client()
            .execute_streaming(HttpRequest::get(format!("http://{address}/media")))
            .unwrap();
        assert_eq!(response.status_code, 302);
        assert!(target.accept().is_err());
        server.join().unwrap();
    }
    #[test]
    fn media_admission_rejects_unbounded_or_incomplete_metadata_and_disappearing_messages() {
        let mut r = reference(b"hello");
        r.validate().unwrap();
        r.size = MAX_BYTES as u64 + 1;
        assert!(r.validate().is_err());
        r.size = 5;
        r.path = "//untrusted.example/file".into();
        assert!(r.validate().is_err());
        r.path = "/valid".into();
        r.key.clear();
        assert!(r.validate().is_err());
        let mut writer = BoundedWriter::default();
        writer.seek(SeekFrom::Start(MAX_BYTES as u64 - 1)).unwrap();
        assert!(writer.write_all(b"xx").is_err());
        writer.truncate(0).unwrap();
        assert!(writer.0.get_ref().is_empty());
        let msg = wa::Message {
            image_message: Some(wa::message::ImageMessage {
                view_once: Some(true),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        };
        assert!(MediaRef::from_message(&msg).is_err());
    }
    #[test]
    fn uploads_are_private_deduplicated_durable_and_integrity_checked() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let bytes = b"Participants should bring notebooks";
        let r = reference(bytes);
        assert!(s
            .ingest_media("owner@test", "upload", "caption", &r)
            .unwrap());
        assert!(!s
            .ingest_media("owner@test", "upload", "replay", &r)
            .unwrap());
        let work = s.next_control().unwrap().unwrap(); // uploads can be stored without a model
        assert!(s.pending_media(&work).unwrap().is_some());
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let w = s.next_control().unwrap().unwrap();
        assert_eq!(w.user, work.user);
        assert!(s.save_attachment(&w, &r, b"wrong hash").is_err());
        let id = s.save_attachment(&w, &r, bytes).unwrap();
        assert_eq!(s.save_attachment(&w, &r, bytes).unwrap(), id);
        assert_eq!(
            s.read_note(&w.user, &id).unwrap(),
            String::from_utf8_lossy(bytes)
        );
        assert!(d
            .path()
            .join("content/resources")
            .join(&id)
            .join("content.bin")
            .exists());
        assert!(!d.path().join("untrusted.txt").exists());
        assert!(s.pending_media(&w).unwrap().is_none());
        s.ingest("other@test", "1", "hi").unwrap();
        let other = s.next().unwrap().unwrap();
        assert!(s.read_note(&other.user, &id).is_err());
        assert!(s.notes(&other.user).unwrap().as_array().unwrap().is_empty());
        assert!(!serde_json::to_string(&s.history(&other.user).unwrap())
            .unwrap()
            .contains("Participants"));
        assert!(!serde_json::to_string(&s.history(&w.user).unwrap())
            .unwrap()
            .contains("/v/t62"));
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(
            s.read_note(&w.user, &id).unwrap(),
            String::from_utf8_lossy(bytes)
        );
    }
    #[test]
    fn unlink_cancels_downloads_and_clears_download_credentials() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        s.ingest_media("owner@test", "1", "", &reference(b"hello"))
            .unwrap();
        let w = s.next().unwrap().unwrap();
        s.reset_whatsapp_session("new-session").unwrap();
        assert!(s.pending_media(&w).unwrap().is_none());
        let db = s.db.lock().unwrap();
        let (state, secret): (String, Option<String>) = db
            .query_row(
                "SELECT state,reference FROM inbox_media WHERE inbox_id=?",
                [w.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "cancelled");
        assert!(secret.is_none());
    }
    #[test]
    fn binary_files_use_lfs_and_are_not_exposed_as_model_text() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let bytes = vec![255; 8192];
        let mut r = reference(&bytes);
        r.mime = "image/jpeg".into();
        r.kind = "image".into();
        s.ingest_media("owner@test", "1", "image", &r).unwrap();
        let w = s.next().unwrap().unwrap();
        let id = s.save_attachment(&w, &r, &bytes).unwrap();
        assert!(s.read_note(&w.user, &id).is_err());
        let hash = Store::lfs_oid(&bytes);
        assert_eq!(
            std::fs::read(
                d.path()
                    .join("content/.git/lfs/objects")
                    .join(&hash[..2])
                    .join(&hash[2..4])
                    .join(hash)
            )
            .unwrap(),
            bytes
        );
        assert_eq!(s.notes(&w.user).unwrap()[0]["kind"], "file");
    }
    #[test]
    fn attachment_budgets_are_checked_before_download_and_survive_restart() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let mut r = reference(b"x");
        r.size = MAX_BYTES as u64;
        for i in 0..4 {
            assert!(s
                .ingest_media("owner@test", &i.to_string(), "", &r)
                .unwrap());
        }
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert!(s.ingest_media("owner@test", "5", "", &r).is_err());
    }
}
