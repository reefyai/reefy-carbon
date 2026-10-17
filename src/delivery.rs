use crate::{
    permissions,
    store::{Store, Work},
};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use whatsapp_rust::prelude::*;

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS file_outbox(
        id INTEGER PRIMARY KEY,inbox_id INTEGER NOT NULL REFERENCES inbox(id),resource TEXT NOT NULL REFERENCES resources(id),
        size INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'staged',created INTEGER NOT NULL DEFAULT(unixepoch()),UNIQUE(inbox_id,resource));
        CREATE INDEX IF NOT EXISTS file_outbox_state ON file_outbox(state,id);
        CREATE INDEX IF NOT EXISTS file_outbox_created ON file_outbox(created);
        UPDATE file_outbox SET state='unknown' WHERE state='sending';
        UPDATE file_outbox SET state='cancelled' WHERE state='staged' AND inbox_id NOT IN(SELECT inbox FROM execution_turns WHERE state IN('active','waiting'));")?;
    Ok(())
}

pub struct FileDelivery {
    pub id: i64,
    pub work: Work,
    pub resource: String,
    pub task: Option<String>,
    pub peer: Option<String>,
    pub destination: String,
    pub stanza: Option<String>,
}
impl Store {
    /// Called under the authority guard. Destination and actor never come from the model.
    pub fn queue_file(&self, work: &Work, resource: &str) -> Result<Value> {
        self.check_work(work)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox WHERE id=? AND user_id=? AND recipient=? AND state='processing')",params![work.id,work.user,work.recipient],|r|r.get::<_,bool>(0))?,"Invalid work");
        ensure!(
            permissions::can_read(&db, &work.user, resource)?,
            "Resource unavailable"
        );
        let (size,kind):(i64,String)=db.query_row("SELECT r.size,m.kind FROM resources r JOIN resource_meta m ON m.resource=r.id WHERE r.id=?",[resource],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            kind != "folder" && size <= crate::media::MAX_BYTES as i64,
            "Not a sendable file"
        );
        if let Some(state) = db
            .query_row(
                "SELECT state FROM file_outbox WHERE inbox_id=? AND resource=?",
                params![work.id, resource],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(
                json!({"resource":resource,"state":state,"destination":"current participant"}),
            );
        }
        let count: i64 = db.query_row(
            "SELECT (SELECT COUNT(*) FROM file_outbox WHERE inbox_id=?1)+(SELECT COUNT(*) FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task WHERE t.inbox_id=?1)",
            [work.id],
            |r| r.get(0),
        )?;
        ensure!(count < 3, "At most three files per request");
        crate::scoped_resources::outbound_budget(&db, &work.user, size)?;
        db.execute(
            "INSERT INTO file_outbox(inbox_id,resource,size) VALUES(?,?,?)",
            params![work.id, resource, size],
        )?;
        db.commit()?;
        Ok(
            json!({"resource":resource,"state":"staged","destination":"current participant","notice":"Will be queued when this reply completes. Do not claim delivery succeeded."}),
        )
    }
    pub fn cancel_staged_files(&self, work: &Work) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE file_outbox SET state='cancelled' WHERE inbox_id=? AND state='staged'",
            [work.id],
        )?;
        Ok(())
    }
    pub fn claim_file(&self) -> Result<Option<FileDelivery>> {
        let _authority = self.authority.lock().unwrap();
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        loop {
            let row=db.query_row("SELECT f.id,f.inbox_id,i.user_id,i.recipient,f.resource,f.task,f.peer,COALESCE((SELECT identity FROM identities WHERE user_id=f.peer),i.recipient),f.stanza FROM file_outbox f JOIN inbox i ON i.id=f.inbox_id WHERE f.state='pending' AND i.state='done' ORDER BY f.id LIMIT 1",[],|r|Ok(FileDelivery{id:r.get(0)?,work:Work{id:r.get(1)?,user:r.get(2)?,recipient:r.get(3)?,text:String::new()},resource:r.get(4)?,task:r.get(5)?,peer:r.get(6)?,destination:r.get(7)?,stanza:r.get(8)?})).optional()?;
            let Some(item) = row else {
                db.commit()?;
                return Ok(None);
            };
            if !valid(&db, &item)? {
                db.execute(
                    "UPDATE file_outbox SET state='cancelled' WHERE id=?",
                    [item.id],
                )?;
                continue;
            }
            db.execute(
                "UPDATE file_outbox SET state='sending' WHERE id=?",
                [item.id],
            )?;
            db.commit()?;
            return Ok(Some(item));
        }
    }
    pub fn check_file_delivery(&self, item: &FileDelivery) -> Result<()> {
        let db = self.db.lock().unwrap();
        let sending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM file_outbox WHERE id=? AND inbox_id=? AND resource=? AND task IS ? AND peer IS ? AND state='sending')",params![item.id,item.work.id,item.resource,item.task,item.peer],|r|r.get(0))?;
        ensure!(
            sending && valid(&db, item)?,
            "File delivery no longer authorized"
        );
        Ok(())
    }
    pub fn file_delivery_result(&self, id: i64, state: &str) -> Result<()> {
        ensure!(
            ["sent", "failed", "unknown", "cancelled"].contains(&state),
            "Invalid delivery state"
        );
        self.db.lock().unwrap().execute(
            "UPDATE file_outbox SET state=? WHERE id=? AND state='sending'",
            params![state, id],
        )?;
        Ok(())
    }
}
fn valid(db: &Connection, item: &FileDelivery) -> Result<bool> {
    let bound:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id JOIN work_epochs w ON w.inbox_id=i.id WHERE i.id=?1 AND i.user_id=?2 AND i.recipient=?3 AND i.state='done' AND i.created>=unixepoch()-86400 AND w.epoch=?4)",params![item.work.id,item.work.user,item.work.recipient,permissions::epoch(db)?],|r|r.get(0))?;
    if !bound {
        return Ok(false);
    }
    match (&item.task, &item.peer) {
        (Some(task), Some(peer)) => {
            let channel:Option<String>=db.query_row("SELECT c.conversation FROM agent_task_channels c JOIN agent_tasks t ON t.id=c.task WHERE c.task=? AND c.peer=? AND t.owner=? AND t.state='open'",params![task,peer,item.work.user],|r|r.get(0)).optional()?;
            Ok(
                channel.is_some_and(|c| crate::coordination::valid(db, &c).unwrap_or(false))
                    && crate::contacts::verified(db, peer).is_ok_and(|id| id == item.destination)
                    && crate::contacts::shareable(db, &item.work.user, peer, &item.resource)?,
            )
        }
        (None, None) => Ok(item.destination == item.work.recipient
            && permissions::can_read(db, &item.work.user, &item.resource)?),
        _ => Ok(false),
    }
}
pub(crate) fn document(
    upload: whatsapp_rust::upload::UploadResponse,
    name: &str,
    kind: &str,
) -> wa::Message {
    let mut name: String = name
        .chars()
        .filter(|c| !c.is_control() && !['/', '\\'].contains(c))
        .take(100)
        .collect();
    if name.trim_matches('.').trim().is_empty() {
        name = "file".into();
    }
    if kind == "note" && !name.ends_with(".md") {
        name.push_str(".md");
    }
    wa::Message {
        document_message: Some(wa::message::DocumentMessage {
            url: Some(upload.url),
            direct_path: Some(upload.direct_path),
            media_key: Some(upload.media_key.to_vec()),
            file_sha256: Some(upload.file_sha256.to_vec()),
            file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
            file_length: Some(upload.file_length),
            media_key_timestamp: Some(upload.media_key_timestamp),
            file_name: Some(name),
            mimetype: Some(
                if kind == "note" {
                    "text/markdown"
                } else {
                    "application/octet-stream"
                }
                .into(),
            ),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    }
}
/// The runtime serializes dispatch with unlink and permission administration.
pub async fn deliver(store: &Store, client: &Client, item: &FileDelivery) -> Result<&'static str> {
    let (name, kind, bytes) = {
        let _authority = store.authority.lock().unwrap();
        store.check_file_delivery(item)?;
        store.read_binary(&item.work.user, &item.resource)?
    };
    let jid: Jid = item.destination.parse()?;
    let uploaded = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        client.upload(
            bytes,
            whatsapp_rust::download::MediaType::Document,
            whatsapp_rust::UploadOptions::default(),
        ),
    )
    .await;
    let upload = match uploaded {
        Ok(Ok(v)) => v,
        _ => return Ok("failed"),
    };
    {
        let _authority = store.authority.lock().unwrap();
        if store.check_file_delivery(item).is_err() {
            return Ok("cancelled");
        }
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        client.send_message_with_options(
            jid,
            document(upload, &name, &kind),
            item.stanza
                .clone()
                .map(|id| whatsapp_rust::send::SendOptions::default().with_message_id(id))
                .unwrap_or_default(),
        ),
    )
    .await;
    Ok(if result.is_ok_and(|r| r.is_ok()) {
        "sent"
    } else {
        "unknown"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn work(s: &Store, identity: &str, id: &str) -> Work {
        s.ingest(identity, id, "Send my file back").unwrap();
        s.next().unwrap().unwrap()
    }
    #[test]
    fn replies_are_bound_to_requester_deduplicated_and_published_with_reply() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = work(&s, "alice@test", "a");
        let b = work(&s, "bob@test", "b");
        let file = s.save_note(&a.user, "notes", "PRIVATE_ALICE").unwrap();
        assert!(s.queue_file(&b, &file).is_err());
        let mut forged = a.clone();
        forged.recipient = b.recipient.clone();
        assert!(s.queue_file(&forged, &file).is_err());
        assert_eq!(s.queue_file(&a, &file).unwrap()["state"], "staged");
        s.queue_file(&a, &file).unwrap();
        assert!(s.claim_file().unwrap().is_none());
        s.finish(&a, "Queued your file.").unwrap();
        let item = s.claim_file().unwrap().unwrap();
        assert_eq!(item.work.recipient, "alice@test");
        assert_eq!(item.resource, file);
        assert_eq!(s.read_binary(&a.user, &file).unwrap().2, b"PRIVATE_ALICE");
        s.check_file_delivery(&item).unwrap();
        s.file_delivery_result(item.id, "sent").unwrap();
        assert!(s.claim_file().unwrap().is_none());
        assert!(s.check_file_delivery(&item).is_err());
        let folder = s.create_folder(&b.user, "folder").unwrap();
        assert!(s.queue_file(&b, &folder).is_err());
    }
    #[test]
    fn revocation_identity_changes_and_unlink_block_dispatch() {
        for mode in ["epoch", "identity", "revoke", "unlink", "expired"] {
            let d = tempfile::tempdir().unwrap();
            let s = Store::open(d.path()).unwrap();
            let a = work(&s, "alice@test", "a");
            let b = work(&s, "bob@test", "b");
            let file = s.save_note(&a.user, "notes", "shared").unwrap();
            s.db.lock()
                .unwrap()
                .execute(
                    "INSERT INTO read_grants(resource,kind,principal) VALUES(?,'user',?)",
                    params![file, b.user],
                )
                .unwrap();
            s.queue_file(&b, &file).unwrap();
            s.finish(&b, "Queued").unwrap();
            let item = s.claim_file().unwrap().unwrap();
            s.check_file_delivery(&item).unwrap();
            match mode {
                "epoch" => {
                    s.db.lock().unwrap().execute("UPDATE settings SET value=CAST(value AS INTEGER)+1 WHERE key='authority_epoch'",[]).unwrap();
                }
                "identity" => {
                    s.db.lock()
                        .unwrap()
                        .execute(
                            "UPDATE identities SET user_id=? WHERE identity=?",
                            params![a.user, b.recipient],
                        )
                        .unwrap();
                }
                "revoke" => {
                    s.db.lock()
                        .unwrap()
                        .execute("DELETE FROM read_grants WHERE resource=?", [&file])
                        .unwrap();
                }
                "expired" => {
                    s.db.lock()
                        .unwrap()
                        .execute(
                            "UPDATE inbox SET created=unixepoch()-86401 WHERE id=?",
                            [b.id],
                        )
                        .unwrap();
                }
                _ => s.reset_whatsapp_session("new").unwrap(),
            }
            assert!(s.check_file_delivery(&item).is_err());
            if mode != "unlink" {
                s.db.lock()
                    .unwrap()
                    .execute(
                        "UPDATE file_outbox SET state='pending' WHERE id=?",
                        [item.id],
                    )
                    .unwrap();
                assert!(s.claim_file().unwrap().is_none());
            }
        }
    }
    #[test]
    fn restart_never_replays_uncertain_or_unfinished_sends() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = work(&s, "alice@test", "a");
        let file = s.save_note(&a.user, "file", "data").unwrap();
        s.queue_file(&a, &file).unwrap();
        s.finish(&a, "queued").unwrap();
        let item = s.claim_file().unwrap().unwrap();
        let unfinished = work(&s, "alice@test", "b");
        s.queue_file(&unfinished, &file).unwrap();
        let pending = work(&s, "alice@test", "c");
        s.queue_file(&pending, &file).unwrap();
        s.finish(&pending, "queued").unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let state: String =
            s.db.lock()
                .unwrap()
                .query_row("SELECT state FROM file_outbox WHERE id=?", [item.id], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(state, "unknown");
        let remaining = s.claim_file().unwrap().unwrap();
        assert_eq!(remaining.work.id, pending.id);
        s.file_delivery_result(remaining.id, "failed").unwrap();
        assert!(s.claim_file().unwrap().is_none());
    }
    #[test]
    fn request_and_daily_limits_include_failed_or_cancelled_attempts() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = work(&s, "a@test", "a");
        let files: Vec<_> = (0..4)
            .map(|n| s.save_note(&a.user, &format!("file{n}"), "data").unwrap())
            .collect();
        for id in &files[..3] {
            s.queue_file(&a, id).unwrap();
        }
        assert!(s.queue_file(&a, &files[3]).is_err());
        s.cancel_staged_files(&a).unwrap();
        s.finish(&a, "Failed request").unwrap();
        assert!(s.claim_file().unwrap().is_none());
        let b = work(&s, "a@test", "b");
        s.db.lock()
            .unwrap()
            .execute("UPDATE file_outbox SET size=32*1024*1024", [])
            .unwrap();
        assert!(s.queue_file(&b, &files[0]).is_err());
    }
}
