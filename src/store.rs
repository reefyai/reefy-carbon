use anyhow::{bail, Context, Result};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

pub struct Store {
    pub(crate) db: Mutex<Connection>,
    pub(crate) authority: Mutex<()>,
    pub(crate) restarting: std::sync::atomic::AtomicBool,
    pub(crate) root: PathBuf,
    _lock: File,
    pub daily_user_limit: i64,
    pub daily_global_limit: i64,
}
#[derive(Clone)]
pub struct Work {
    pub id: i64,
    pub user: String,
    pub recipient: String,
    pub text: String,
}
enum ResourceWrite<'a> {
    New,
    MessageNote(&'a Work),
    DocumentSummary(&'a Work, &'a str),
    Attachment(i64),
    Revision(&'a str),
    Profile(&'a Work, Option<&'a str>),
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("state"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("state/instance.lock"))?;
        lock.try_lock_exclusive()
            .context("This data directory is already in use")?;
        let db = Connection::open(root.join("state/carbon.sqlite"))?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS users(id TEXT PRIMARY KEY,created INTEGER NOT NULL DEFAULT(unixepoch()));
          CREATE TABLE IF NOT EXISTS identities(identity TEXT PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id));
          CREATE TABLE IF NOT EXISTS participant_labels(identity TEXT PRIMARY KEY REFERENCES identities(identity),label TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS inbox(id INTEGER PRIMARY KEY,provider_id TEXT NOT NULL,recipient TEXT NOT NULL,user_id TEXT NOT NULL REFERENCES users(id),text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',created INTEGER NOT NULL DEFAULT(unixepoch()),UNIQUE(recipient,provider_id));
          CREATE INDEX IF NOT EXISTS inbox_user_state ON inbox(user_id,state,id);
          CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id),role TEXT NOT NULL,text TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS outbox(id INTEGER PRIMARY KEY,inbox_id INTEGER UNIQUE NOT NULL REFERENCES inbox(id),recipient TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending');
          CREATE TABLE IF NOT EXISTS quotas(user_id TEXT NOT NULL REFERENCES users(id),day TEXT NOT NULL,used INTEGER NOT NULL,PRIMARY KEY(user_id,day));
          CREATE TABLE IF NOT EXISTS resources(id TEXT PRIMARY KEY,owner TEXT NOT NULL REFERENCES users(id),name TEXT NOT NULL,oid TEXT NOT NULL,commit_id TEXT NOT NULL,size INTEGER NOT NULL,lfs_oid TEXT,created INTEGER NOT NULL DEFAULT(unixepoch()));
          CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY,event TEXT NOT NULL,subject TEXT NOT NULL,created INTEGER NOT NULL DEFAULT(unixepoch()));
          CREATE TABLE IF NOT EXISTS activity(id INTEGER PRIMARY KEY AUTOINCREMENT,created INTEGER NOT NULL DEFAULT(unixepoch()),level TEXT NOT NULL,event TEXT NOT NULL,identity TEXT,message TEXT NOT NULL);

          UPDATE outbox SET state='unknown' WHERE state='sending';")?;
        crate::execution::migrate(&db)?;
        let has_operation = {
            let mut q = db.prepare("PRAGMA table_info(activity)")?;
            let rows = q
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.iter().any(|n| n == "operation_id")
        };
        if !has_operation {
            db.execute("ALTER TABLE activity ADD COLUMN operation_id TEXT", [])?;
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS inbox_operations(inbox_id INTEGER PRIMARY KEY REFERENCES inbox(id),operation_id TEXT NOT NULL UNIQUE);")?;
        crate::permissions::migrate(&db)?;
        crate::private_context::migrate(&db)?;
        crate::document_sharing::migrate(&db)?;
        crate::document_summary::migrate(&db)?;
        crate::message_notes::migrate(&db)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS resource_meta(resource TEXT PRIMARY KEY REFERENCES resources(id),kind TEXT NOT NULL CHECK(kind IN('note','file','folder')),mime TEXT NOT NULL,parent TEXT REFERENCES resources(id));
        INSERT OR IGNORE INTO resource_meta(resource,kind,mime) SELECT id,'note','text/markdown' FROM resources;
        CREATE TABLE IF NOT EXISTS inbox_media(inbox_id INTEGER PRIMARY KEY REFERENCES inbox(id),reference TEXT,state TEXT NOT NULL DEFAULT 'pending',resource TEXT REFERENCES resources(id),size INTEGER NOT NULL,created INTEGER NOT NULL DEFAULT(unixepoch()));
        UPDATE inbox SET state='pending' WHERE state='interrupted' AND id IN(SELECT inbox_id FROM inbox_media WHERE state='pending');
        CREATE INDEX IF NOT EXISTS resource_parent ON resource_meta(parent);
        CREATE INDEX IF NOT EXISTS resource_owner ON resources(owner);
        CREATE INDEX IF NOT EXISTS media_created ON inbox_media(created);")?;
        crate::revisions::migrate(&db)?;
        crate::delivery::migrate(&db)?;
        crate::attachment_llm::migrate(&db)?;
        crate::search::migrate(&db)?;
        crate::progress::migrate(&db)?;
        crate::tasks::migrate(&db)?;
        crate::contacts::migrate(&db)?;
        crate::inquiry_routing::migrate(&db)?;
        let content = root.join("content");
        fs::create_dir_all(&content)?;
        let repo = if content.join(".git").exists() {
            git2::Repository::open(&content)?
        } else {
            git2::Repository::init(&content)?
        };
        // Never run Git hooks or filters: libgit2 writes content directly.
        fs::create_dir_all(repo.path().join("lfs/objects"))?;
        Ok(Self {
            db: Mutex::new(db),
            authority: Mutex::new(()),
            restarting: std::sync::atomic::AtomicBool::new(false),
            root: root.to_owned(),
            _lock: lock,
            daily_user_limit: 50,
            daily_global_limit: 500,
        })
    }
    // Only fixed application messages enter the log, never model output or raw errors.
    pub fn log(
        &self,
        level: &'static str,
        event: &'static str,
        identity: Option<&str>,
        message: &'static str,
    ) {
        self.log_operation(None, level, event, identity, message);
    }
    pub fn log_work(
        &self,
        inbox: i64,
        level: &'static str,
        event: &'static str,
        identity: Option<&str>,
        message: &'static str,
    ) {
        let operation = (|| -> Result<String> {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT OR IGNORE INTO inbox_operations VALUES(?,?)",
                params![inbox, Uuid::new_v4().to_string()],
            )?;
            Ok(db.query_row(
                "SELECT operation_id FROM inbox_operations WHERE inbox_id=?",
                [inbox],
                |r| r.get(0),
            )?)
        })()
        .ok();
        self.log_operation(operation.as_deref(), level, event, identity, message);
    }
    pub fn log_incoming(
        &self,
        recipient: &str,
        provider: &str,
        level: &'static str,
        event: &'static str,
        identity: Option<&str>,
        message: &'static str,
    ) {
        let inbox = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM inbox WHERE recipient=? AND provider_id=?",
                params![recipient, provider],
                |r| r.get::<_, i64>(0),
            )
            .ok();
        if let Some(inbox) = inbox {
            self.log_work(inbox, level, event, identity, message)
        } else {
            self.log(level, event, identity, message)
        }
    }
    pub fn log_outbox(
        &self,
        outbox: i64,
        level: &'static str,
        event: &'static str,
        identity: Option<&str>,
        message: &'static str,
    ) {
        let inbox = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT inbox_id FROM outbox WHERE id=?", [outbox], |r| {
                r.get::<_, i64>(0)
            })
            .ok();
        if let Some(inbox) = inbox {
            self.log_work(inbox, level, event, identity, message)
        } else {
            self.log(level, event, identity, message)
        }
    }
    fn log_operation(
        &self,
        operation: Option<&str>,
        level: &'static str,
        event: &'static str,
        identity: Option<&str>,
        message: &'static str,
    ) {
        let identity = identity.map(|v| v.chars().take(128).collect::<String>());
        eprintln!(
            "{}",
            json!({"level":level,"event":event,"identity":identity,"message":message,"operation_id":operation})
        );
        let result = (|| -> Result<()> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO activity(level,event,identity,message,operation_id) VALUES(?,?,?,?,?)",
                params![level, event, identity, message, operation],
            )?;
            tx.execute("DELETE FROM activity WHERE created < unixepoch()-604800 OR id <= (SELECT COALESCE(MAX(id),0)-1000 FROM activity)", [])?;
            tx.commit()?;
            Ok(())
        })();
        if result.is_err() {
            eprintln!("Activity log could not be persisted");
        }
    }
    pub fn activity(&self) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare("SELECT id,created,level,event,identity,message,operation_id FROM activity WHERE created >= unixepoch()-604800 ORDER BY id DESC LIMIT 1000")?;
        let rows = query.query_map([], |r| Ok(json!({"id":r.get::<_,i64>(0)?,"created":r.get::<_,i64>(1)?,"level":r.get::<_,String>(2)?,"event":r.get::<_,String>(3)?,"identity":r.get::<_,Option<String>>(4)?,"message":r.get::<_,String>(5)?,"operation_id":r.get::<_,Option<String>>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"events":rows}))
    }
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn reset_whatsapp_session(&self, session: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("INSERT INTO settings(key,value) VALUES('whatsapp_session',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [session])?;
        tx.execute("DELETE FROM settings WHERE key='whatsapp_logout'", [])?;
        tx.execute(
            "UPDATE inbox SET state='cancelled' WHERE state IN ('pending','processing')",
            [],
        )?;
        tx.execute(
            "UPDATE outbox SET state='cancelled' WHERE state IN ('pending','sending')",
            [],
        )?;
        tx.execute(
            "UPDATE task_contact_consents SET state='revoked' WHERE state IN('staged','active')",
            [],
        )?;
        tx.execute(
            "UPDATE contact_requests SET state='cancelled' WHERE state IN('staged','pending')",
            [],
        )?;
        tx.execute("UPDATE inquiry_clarifications SET state='cancelled' WHERE state IN('staged','pending','resolving')",[])?;
        tx.execute("DELETE FROM inquiry_pending_wakes", [])?;
        tx.execute(
            "UPDATE conversations SET state='closed' WHERE state='open'",
            [],
        )?;
        tx.execute("UPDATE conversation_files SET state='cancelled' WHERE state IN('staged','pending','uploading','sending')",[])?;
        tx.execute("UPDATE conversation_tasks SET state='cancelled' WHERE state IN ('staged','queued','processing','sending')", [])?;
        tx.execute("UPDATE agent_task_messages SET state='cancelled' WHERE state IN('staged','pending','sending')",[])?;
        tx.execute(
            "UPDATE agent_task_wakes SET state='cancelled' WHERE state='pending'",
            [],
        )?;
        tx.execute("UPDATE file_outbox SET state='cancelled' WHERE state IN ('staged','pending','sending')", [])?;
        tx.execute(
            "UPDATE control_outbox SET state='cancelled' WHERE state IN ('pending','sending')",
            [],
        )?;
        tx.execute(
            "UPDATE inbox_media SET state='cancelled',reference=NULL WHERE state='pending'",
            [],
        )?;
        tx.execute(
            "UPDATE proposals SET state='cancelled' WHERE state='pending'",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_whatsapp_logout(&self, reason: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO settings(key,value) VALUES('whatsapp_logout',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [reason],
        )?;
        Ok(())
    }

    pub fn set_model(&self, model: &str) -> Result<()> {
        self.db.lock().unwrap().execute("INSERT INTO settings VALUES('model',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[model])?;
        Ok(())
    }
    /// Native WhatsApp display metadata only. Never used for identity resolution or authority.
    pub fn cache_identity_label(&self, identity: &str, label: &str) -> Result<()> {
        if label.len() > 400 || label.chars().any(char::is_control) {
            bail!("Invalid display label")
        }
        self.db.lock().unwrap().execute("INSERT INTO participant_labels(identity,label) SELECT identity,?2 FROM identities WHERE identity=?1 ON CONFLICT(identity) DO UPDATE SET label=excluded.label",params![identity,label])?;
        Ok(())
    }
    pub fn participants(&self) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let mut s =
            db.prepare("SELECT user_id,identity FROM identities ORDER BY rowid LIMIT 1000")?;
        let rows = s
            .query_map([], |r| {
                Ok(json!({"id":r.get::<_,String>(0)?,"identity":r.get::<_,String>(1)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!(rows))
    }
    pub fn bootstrap_admins(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty()
            || ids.len() > 1000
            || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
        {
            bail!("Choose one or more distinct verified participants")
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if tx.query_row(
            "SELECT COUNT(*) FROM settings WHERE key='true_admins'",
            [],
            |r| r.get::<_, i64>(0),
        )? != 0
        {
            bail!("Admin bootstrap is already sealed")
        }
        for id in ids {
            if tx.query_row(
                "SELECT COUNT(*) FROM identities WHERE user_id=?",
                [id],
                |r| r.get::<_, i64>(0),
            )? != 1
            {
                bail!("Participant has not been verified by an incoming message")
            }
        }
        tx.execute(
            "INSERT INTO settings VALUES('true_admins',?)",
            [serde_json::to_string(ids)?],
        )?;
        tx.execute(
            "INSERT INTO audit(event,subject) VALUES('bootstrap_admins',?)",
            [serde_json::to_string(ids)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn ingest(&self, identity: &str, provider_id: &str, text: &str) -> Result<bool> {
        self.ingest_transport(identity, provider_id, text, true)
    }
    #[cfg(test)]
    pub fn ingest_transport(
        &self,
        identity: &str,
        provider_id: &str,
        text: &str,
        control_allowed: bool,
    ) -> Result<bool> {
        self.ingest_reply(identity, provider_id, text, control_allowed, None)
    }
    pub fn ingest_reply(
        &self,
        identity: &str,
        provider_id: &str,
        text: &str,
        control_allowed: bool,
        quoted: Option<&str>,
    ) -> Result<bool> {
        self.ingest_inner(identity, provider_id, text, control_allowed, None, quoted)
    }
    #[cfg(test)]
    pub fn ingest_media(
        &self,
        identity: &str,
        provider_id: &str,
        caption: &str,
        media: &crate::media::MediaRef,
    ) -> Result<bool> {
        self.ingest_media_reply(identity, provider_id, caption, media, false, None)
    }
    pub fn ingest_media_reply(
        &self,
        identity: &str,
        provider_id: &str,
        caption: &str,
        media: &crate::media::MediaRef,
        allowed: bool,
        quoted: Option<&str>,
    ) -> Result<bool> {
        media.validate()?;
        self.ingest_inner(
            identity,
            provider_id,
            &format!(
                "Attachment received. Caption: {}",
                caption.chars().take(4000).collect::<String>()
            ),
            allowed,
            Some(media),
            quoted,
        )
    }
    fn ingest_inner(
        &self,
        identity: &str,
        provider_id: &str,
        text: &str,
        control_allowed: bool,
        media: Option<&crate::media::MediaRef>,
        quoted: Option<&str>,
    ) -> Result<bool> {
        if text.is_empty() || text.len() > 8192 || identity.len() > 200 || provider_id.len() > 200 {
            bail!("Message outside size limits")
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if tx.query_row(
            "SELECT COUNT(*) FROM inbox WHERE recipient=? AND provider_id=?",
            params![identity, provider_id],
            |r| r.get::<_, i64>(0),
        )? > 0
        {
            return Ok(false);
        }
        let known: Option<String> = tx
            .query_row(
                "SELECT user_id FROM identities WHERE identity=?",
                [identity],
                |r| r.get(0),
            )
            .optional()?;
        let user = match known {
            Some(id) => id,
            None => {
                if tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get::<_, i64>(0))? >= 1000 {
                    bail!("Participant limit reached")
                }
                let id = Uuid::new_v4().to_string();
                tx.execute("INSERT INTO users(id) VALUES(?)", [&id])?;
                tx.execute("INSERT INTO identities VALUES(?,?)", params![identity, id])?;
                id
            }
        };
        let used: i64 = tx.query_row(
            "SELECT COALESCE(SUM(used),0) FROM quotas WHERE day=date('now')",
            [],
            |r| r.get(0),
        )?;
        let personal: i64 = tx
            .query_row(
                "SELECT used FROM quotas WHERE user_id=? AND day=date('now')",
                [&user],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if let Some(media) = media {
            let (personal,total):(i64,i64)=tx.query_row("SELECT COALESCE(SUM(CASE WHEN i.user_id=? THEN m.size ELSE 0 END),0),COALESCE(SUM(m.size),0) FROM inbox_media m JOIN inbox i ON i.id=m.inbox_id WHERE m.created>unixepoch()-86400",[&user],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if personal + media.size as i64 > 32 * 1024 * 1024
                || total + media.size as i64 > 256 * 1024 * 1024
            {
                bail!("Daily attachment budget reached")
            }
        }
        let control_lane = control_allowed
            && ((crate::permissions::control_verb(text)
                && crate::permissions::reviewers(&tx)?.contains(&user))
                || crate::coordination::stop_allowed(&tx, &user, identity, text, quoted)?
                || crate::tasks::stop_allowed(&tx, &user, text)?
                || (crate::permissions::control_verb(text) && quoted.is_some_and(|quote| tx.query_row("SELECT EXISTS(SELECT 1 FROM contact_requests WHERE stanza=? AND owner=? AND owner_identity=? AND state='pending')",params![quote,user,identity],|r|r.get::<_,bool>(0)).unwrap_or(false))));
        if !control_lane && (personal >= self.daily_user_limit || used >= self.daily_global_limit) {
            bail!("Daily request limit reached")
        }
        tx.execute("INSERT INTO quotas VALUES(?,date('now'),1) ON CONFLICT(user_id,day) DO UPDATE SET used=used+1",[&user])?;
        tx.execute(
            "INSERT INTO inbox(provider_id,recipient,user_id,text) VALUES(?,?,?,?)",
            params![provider_id, identity, user, text],
        )?;
        let inbox_id = tx.last_insert_rowid();
        if let Some(stanza) = quoted.filter(|s| !s.is_empty() && s.len() <= 200) {
            tx.execute(
                "INSERT INTO approval_quotes VALUES(?,?)",
                params![inbox_id, stanza],
            )?;
        }
        if let Some(media) = media {
            tx.execute(
                "INSERT INTO inbox_media(inbox_id,reference,size) VALUES(?,?,?)",
                params![inbox_id, serde_json::to_string(media)?, media.size as i64],
            )?;
        }
        if !control_allowed {
            tx.execute(
                "INSERT INTO blocked_controls(inbox_id) VALUES(?)",
                [inbox_id],
            )?;
        }
        tx.execute(
            "INSERT INTO inbox_operations VALUES(?,?)",
            params![inbox_id, Uuid::new_v4().to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }
    #[cfg(test)]
    pub fn next(&self) -> Result<Option<Work>> {
        self.next_kind(false, false)
    }
    #[cfg(test)]
    pub fn next_control(&self) -> Result<Option<Work>> {
        self.next_kind(true, false)
    }
    /// Atomic production claim: serialize one participant across private and pair work.
    pub(crate) fn claim_private(&self, controls_only: bool) -> Result<Option<Work>> {
        self.next_kind(controls_only, true)
    }
    fn next_kind(&self, controls_only: bool, serialized: bool) -> Result<Option<Work>> {
        if self.restarting.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(None);
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if self.restarting.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(None);
        }
        let w=tx.query_row("SELECT id,user_id,recipient,text FROM inbox WHERE state='pending' AND NOT EXISTS(SELECT 1 FROM execution_turns j WHERE j.inbox=inbox.id AND j.due>unixepoch() AND j.state='waiting') AND (?2=0 OR (NOT EXISTS(SELECT 1 FROM inbox busy WHERE busy.user_id=inbox.user_id AND (busy.state='processing' OR (busy.id<inbox.id AND EXISTS(SELECT 1 FROM execution_turns j WHERE j.inbox=busy.id AND j.state IN('active','waiting') AND busy.state='pending')))) AND NOT EXISTS(SELECT 1 FROM conversation_tasks t JOIN conversations c ON c.id=t.conversation WHERE t.state IN('processing','sending') AND inbox.user_id IN(c.a,c.b)))) AND (?1=0 OR id IN(SELECT inbox_id FROM inbox_media WHERE state='pending') OR lower(trim(text)) GLOB 'approve *' OR lower(trim(text)) GLOB 'approving *' OR lower(trim(text)) GLOB 'reject *' OR lower(trim(text)) GLOB 'rejecting *' OR lower(trim(text)) IN ('approve','approving','reject','rejecting') OR (lower(trim(text))='stop' AND id IN(SELECT q.inbox_id FROM approval_quotes q JOIN conversation_deliveries t ON t.stanza=q.stanza_id))) ORDER BY EXISTS(SELECT 1 FROM agent_task_events e WHERE e.inbox=inbox.id),id LIMIT 1",params![controls_only,serialized],|r|Ok(Work{id:r.get(0)?,user:r.get(1)?,recipient:r.get(2)?,text:r.get(3)?})).optional()?;
        if let Some(w) = &w {
            tx.execute(
                "INSERT OR REPLACE INTO work_epochs VALUES(?,?)",
                params![w.id, crate::permissions::epoch(&tx)?],
            )?;
            tx.execute("UPDATE inbox SET state='processing' WHERE id=?", [w.id])?;
            if !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM execution_turns WHERE inbox=?)",
                [w.id],
                |r| r.get::<_, bool>(0),
            )? {
                tx.execute(
                    "INSERT INTO messages(user_id,role,text) VALUES(?,'user',?)",
                    params![w.user, w.text],
                )?;
            }
        }
        tx.commit()?;
        Ok(w)
    }
    pub fn history(&self, user: &str) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut s=db.prepare("SELECT role,text FROM (SELECT id,role,text FROM messages WHERE user_id=? AND id>CAST((SELECT value FROM settings WHERE key='history_floor') AS INTEGER) ORDER BY id DESC LIMIT 16) ORDER BY id")?;
        let rows = s
            .query_map([user], |r| {
                Ok(json!({"role":r.get::<_,String>(0)?,"content":r.get::<_,String>(1)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn finish(&self, w: &Work, text: &str) -> Result<()> {
        let _authority = self.authority.lock().unwrap();
        self.check_work(w)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let notice: Option<String> = tx
            .query_row(
                "SELECT summary FROM contact_requests WHERE source=? AND state='staged'",
                [w.id],
                |r| r.get(0),
            )
            .optional()?;
        let clarification: Option<String> = tx
            .query_row(
                "SELECT question FROM inquiry_clarifications WHERE inbox=? AND state='staged'",
                [w.id],
                |r| r.get(0),
            )
            .optional()?;
        if clarification.is_some() {
            // A held input may publish its factual question, never staged outreach or files.
            tx.execute("UPDATE agent_task_messages SET state='cancelled' WHERE source=? AND state='staged'",[w.id])?;
            tx.execute("UPDATE conversation_tasks SET state='cancelled' WHERE inbox_id=? AND state='staged'",[w.id])?;
            tx.execute(
                "UPDATE file_outbox SET state='cancelled' WHERE inbox_id=? AND state='staged'",
                [w.id],
            )?;
            tx.execute(
                "UPDATE agent_task_wakes SET state='cancelled' WHERE source=? AND state='pending'",
                [w.id],
            )?;
            tx.execute("UPDATE task_contact_consents SET state='revoked' WHERE source=? AND state='staged'",[w.id])?;
            tx.execute(
                "UPDATE contact_requests SET state='cancelled' WHERE source=? AND state='staged'",
                [w.id],
            )?;
        }
        let text = clarification
            .as_deref()
            .or(notice.as_deref())
            .unwrap_or(text);
        tx.execute(
            "UPDATE inquiry_clarifications SET state='pending' WHERE inbox=? AND state='staged'",
            [w.id],
        )?;
        crate::inquiry_routing::publish_turn(
            &tx,
            w,
            self.daily_user_limit,
            self.daily_global_limit,
        )?;
        tx.execute(
            "UPDATE contact_requests SET state='pending' WHERE source=? AND state='staged'",
            [w.id],
        )?;
        tx.execute(
            "UPDATE task_contact_consents SET state='active' WHERE source=? AND state='staged'",
            [w.id],
        )?;
        tx.execute("INSERT OR IGNORE INTO private_context_sources SELECT ?1,resource FROM context_sources WHERE user_id=?2 AND EXISTS(SELECT 1 FROM private_context_turns WHERE inbox=?1)",params![w.id,w.user])?;
        tx.execute(
            "INSERT INTO messages(user_id,role,text) VALUES(?,'assistant',?)",
            params![w.user, text],
        )?;
        tx.execute(
            "INSERT INTO outbox(inbox_id,recipient,text) VALUES(?,?,?)",
            params![w.id, w.recipient, text],
        )?;
        tx.execute("UPDATE inbox SET state='done' WHERE id=?", [w.id])?;
        tx.execute(
            "UPDATE conversation_tasks SET state='queued' WHERE inbox_id=? AND state='staged'",
            [w.id],
        )?;
        tx.execute(
            "UPDATE file_outbox SET state='pending' WHERE inbox_id=? AND state='staged'",
            [w.id],
        )?;
        tx.execute(
            "UPDATE agent_task_messages SET state='pending' WHERE source=? AND state='staged'",
            [w.id],
        )?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_task_routes WHERE inbox=?1) OR EXISTS(SELECT 1 FROM inquiry_clarifications WHERE resolution_turn=?1 AND state='resolved')",
            [w.id],
            |r| r.get::<_, bool>(0),
        )? || text.trim() == "[WAIT]"
        {
            tx.execute(
                "DELETE FROM outbox WHERE inbox_id=? AND state='pending'",
                [w.id],
            )?;
        }
        tx.execute(
            "UPDATE execution_turns SET state='done' WHERE inbox=?",
            [w.id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn claim_send(&self) -> Result<Option<(i64, String, String)>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("UPDATE outbox SET state='cancelled' WHERE state='pending' AND inbox_id IN (SELECT inbox_id FROM work_epochs WHERE epoch<>?)", [crate::permissions::epoch(&tx)?])?;
        tx.execute("UPDATE inquiry_clarifications SET state='expired' WHERE state='pending' AND expires<=unixepoch()",[])?;
        tx.execute("UPDATE inquiry_clarifications SET state='cancelled' WHERE state='pending' AND NOT EXISTS(SELECT 1 FROM identities WHERE user_id=inquiry_clarifications.participant AND identity=inquiry_clarifications.identity)",[])?;
        tx.execute("UPDATE outbox SET state='cancelled' WHERE state='pending' AND inbox_id IN(SELECT inbox FROM inquiry_clarifications WHERE state IN('resolved','cancelled','expired'))",[])?;
        tx.execute("UPDATE contact_requests SET state='expired' WHERE state='pending' AND expires<=unixepoch()",[])?;
        tx.execute("UPDATE contact_requests SET state='stale' WHERE state='pending' AND (NOT EXISTS(SELECT 1 FROM agent_tasks t WHERE t.id=contact_requests.task AND t.owner=contact_requests.owner AND t.state='open') OR NOT EXISTS(SELECT 1 FROM identities WHERE user_id=contact_requests.owner AND identity=contact_requests.owner_identity) OR NOT EXISTS(SELECT 1 FROM identities WHERE user_id=contact_requests.peer AND identity=contact_requests.peer_identity) OR EXISTS(SELECT 1 FROM contact_blocks b WHERE (b.owner=contact_requests.owner AND b.peer=contact_requests.peer) OR (b.owner=contact_requests.peer AND b.peer=contact_requests.owner)))",[])?;
        tx.execute("UPDATE outbox SET state='cancelled' WHERE state='pending' AND inbox_id IN(SELECT source FROM contact_requests WHERE state<>'pending')",[])?;
        let row = tx
            .query_row(
                "SELECT id,recipient,text FROM outbox WHERE state='pending' ORDER BY id LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, _, _)) = &row {
            tx.execute("UPDATE outbox SET state='sending' WHERE id=?", [id])?;
        }
        tx.commit()?;
        Ok(row)
    }
    pub fn sent(&self, id: i64, success: bool) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE outbox SET state=? WHERE id=?",
            params![if success { "sent" } else { "unknown" }, id],
        )?;
        Ok(())
    }
    pub fn notes(&self, user: &str) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let mut q=db.prepare("WITH RECURSIVE visible(id) AS (SELECT r.id FROM resources r WHERE r.owner=?1 OR EXISTS(SELECT 1 FROM read_grants g WHERE g.resource=r.id AND (g.kind='everyone' OR (g.kind='user' AND g.principal=?1) OR (g.kind='group' AND EXISTS(SELECT 1 FROM memberships m WHERE m.group_id=g.principal AND m.user_id=?1)))) UNION SELECT m.resource FROM resource_meta m JOIN visible v ON m.parent=v.id) SELECT r.id,r.name,m.kind,m.mime,r.size,m.parent FROM visible v JOIN resources r ON r.id=v.id JOIN resource_meta m ON m.resource=r.id ORDER BY r.created DESC,r.id LIMIT 100")?;
        let candidates = q
            .query_map([user], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut rows = Vec::new();
        for (id, name, kind, mime, size, parent) in candidates {
            if crate::permissions::can_read(&db, user, &id)?
                && crate::revisions::latest_visible(&db, user, &id)?
            {
                db.execute(
                    "INSERT OR IGNORE INTO context_sources VALUES(?,?)",
                    params![user, id],
                )?;
                rows.push(json!({"id":id,"name":name,"kind":kind,"mime":mime,"size":size,"parent":parent}));
            }
        }
        Ok(json!(rows))
    }
    pub fn read_note(&self, user: &str, id: &str) -> Result<String> {
        let db = self.db.lock().unwrap();
        if !crate::permissions::can_read(&db, user, id)? {
            bail!("Resource unavailable");
        }
        let (kind,mime,size):(String,String,i64)=db.query_row("SELECT m.kind,m.mime,r.size FROM resource_meta m JOIN resources r ON r.id=m.resource WHERE r.id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if kind == "folder"
            || size > 16384
            || (kind == "file"
                && ![
                    "text/plain",
                    "text/markdown",
                    "text/csv",
                    "application/json",
                ]
                .contains(&mime.as_str()))
        {
            bail!("This resource cannot be read as bounded UTF-8 text")
        }
        db.execute(
            "INSERT OR IGNORE INTO context_sources VALUES(?,?)",
            params![user, id],
        )?;
        let row: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT oid,lfs_oid FROM resources WHERE id=?",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (oid, lfs_oid) = row.context("Resource unavailable")?;
        if let Some(hash) = lfs_oid {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("Invalid resource digest")
            }
            let bytes = fs::read(
                self.root
                    .join("content/.git/lfs/objects")
                    .join(&hash[..2])
                    .join(&hash[2..4])
                    .join(&hash),
            )?;
            if Self::lfs_oid(&bytes) != hash {
                bail!("Resource integrity check failed")
            }
            return Ok(String::from_utf8(bytes)?);
        }
        let repo = git2::Repository::open(self.root.join("content"))?;
        let blob = repo.find_blob(git2::Oid::from_str(&oid)?)?;
        Ok(String::from_utf8(blob.content().to_vec())?)
    }
    /// Binary retrieval is authorized against SQLite and verified against immutable storage.
    /// Only this participant's context is tainted; no filesystem paths enter model arguments.
    pub fn read_binary(&self, user: &str, id: &str) -> Result<(String, String, Vec<u8>)> {
        let db = self.db.lock().unwrap();
        if !crate::permissions::can_read(&db, user, id)? {
            bail!("Resource unavailable")
        }
        let result = self.verified_bytes(&db, id)?;
        db.execute(
            "INSERT OR IGNORE INTO context_sources VALUES(?,?)",
            params![user, id],
        )?;
        Ok(result)
    }
    pub(crate) fn verified_bytes(
        &self,
        db: &Connection,
        id: &str,
    ) -> Result<(String, String, Vec<u8>)> {
        use std::io::Read;
        let (kind, size, oid, lfs, name): (String, i64, String, Option<String>, String) = db.query_row(
            "SELECT m.kind,r.size,r.oid,r.lfs_oid,r.name FROM resources r JOIN resource_meta m ON m.resource=r.id WHERE r.id=?",
            [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        if kind == "folder" || size < 0 || size > crate::media::MAX_BYTES as i64 {
            bail!("Unsupported file")
        }
        let bytes = if let Some(hash) = lfs {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("Invalid resource digest")
            }
            let path = self
                .root
                .join("content/.git/lfs/objects")
                .join(&hash[..2])
                .join(&hash[2..4])
                .join(&hash);
            let mut bytes = Vec::new();
            File::open(path)?
                .take(crate::media::MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if Self::lfs_oid(&bytes) != hash {
                bail!("Resource integrity check failed")
            }
            bytes
        } else {
            let repo = git2::Repository::open(self.root.join("content"))?;
            let blob = repo.find_blob(git2::Oid::from_str(&oid)?)?;
            if blob.size() > crate::media::MAX_BYTES {
                bail!("Resource exceeds limit")
            }
            let bytes = blob.content().to_vec();
            if git2::Oid::hash_object(git2::ObjectType::Blob, &bytes)?.to_string() != oid {
                bail!("Resource integrity check failed")
            }
            bytes
        };
        if bytes.len() as i64 != size {
            bail!("Resource size mismatch")
        }
        Ok((name, kind, bytes))
    }
    #[cfg(test)]
    pub fn read_image(&self, user: &str, id: &str) -> Result<(String, Vec<u8>)> {
        let (_, kind, bytes) = self.read_binary(user, id)?;
        if kind != "file" {
            bail!("Unsupported image")
        }
        Ok((crate::media::image_mime(&bytes)?.into(), bytes))
    }
    pub fn save_note(&self, user: &str, name: &str, text: &str) -> Result<String> {
        if name.is_empty() || name.len() > 120 || text.len() > 16384 {
            bail!("Note exceeds limits")
        }
        self.save_resource(
            user,
            name,
            text.as_bytes(),
            "note",
            "text/markdown",
            ResourceWrite::New,
        )
    }
    pub(crate) fn save_isolated_message_note(
        &self,
        work: &Work,
        name: &str,
        text: &str,
    ) -> Result<String> {
        anyhow::ensure!(
            !name.trim().is_empty()
                && name.len() <= 120
                && !text.trim().is_empty()
                && text.len() <= 12000,
            "Message note outside limits"
        );
        self.save_resource(
            &work.user,
            name,
            text.as_bytes(),
            "note",
            "text/markdown",
            ResourceWrite::MessageNote(work),
        )
    }
    // Only the isolated document model path supplies this text; never a tool draft.
    pub(crate) fn save_isolated_document_summary(
        &self,
        work: &Work,
        source: &str,
        text: &str,
    ) -> Result<String> {
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= 12000,
            "Attachment response outside limits"
        );
        let mut name = {
            let db = self.db.lock().unwrap();
            crate::document_summary::authorized(&db, work, source)?;
            let original: String =
                db.query_row("SELECT name FROM resources WHERE id=?", [source], |r| {
                    r.get(0)
                })?;
            format!("Summary — {original}")
        };
        while name.len() > 120 {
            name.pop();
        }
        self.save_resource(
            &work.user,
            &name,
            text.as_bytes(),
            "note",
            "text/markdown",
            ResourceWrite::DocumentSummary(work, source),
        )
    }
    pub fn revise_note(&self, user: &str, base: &str, name: &str, text: &str) -> Result<String> {
        if name.trim().is_empty() || name.len() > 120 || text.len() > 16384 {
            bail!("Note exceeds limits")
        }
        self.save_resource(
            user,
            name,
            text.as_bytes(),
            "note",
            "text/markdown",
            ResourceWrite::Revision(base),
        )
    }
    pub fn create_folder(&self, user: &str, name: &str) -> Result<String> {
        self.save_resource(
            user,
            name,
            b"",
            "folder",
            "application/x-carbon-folder",
            ResourceWrite::New,
        )
    }
    pub fn save_attachment(
        &self,
        w: &Work,
        media: &crate::media::MediaRef,
        bytes: &[u8],
    ) -> Result<String> {
        media.validate()?;
        if let Some(expected) = self.pending_media(w)? {
            if &expected != media {
                bail!("Media reference mismatch")
            }
        }
        if bytes.len() as u64 != media.size
            || Sha256::digest(bytes).as_slice() != media.sha.as_slice()
        {
            bail!("Attachment integrity mismatch")
        }
        self.save_resource(
            &w.user,
            &media.name,
            bytes,
            "file",
            &media.mime,
            ResourceWrite::Attachment(w.id),
        )
    }
    fn save_resource(
        &self,
        user: &str,
        name: &str,
        bytes: &[u8],
        kind: &str,
        mime: &str,
        mode: ResourceWrite<'_>,
    ) -> Result<String> {
        if name.is_empty() || name.len() > 400 || bytes.len() > crate::media::MAX_BYTES {
            bail!("Resource exceeds limits")
        }
        // Holding this lock serializes publication. No network or model work is allowed here.
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        let inbox = if let ResourceWrite::Attachment(id) = mode {
            Some(id)
        } else {
            None
        };
        if let ResourceWrite::MessageNote(work) = mode {
            anyhow::ensure!(user == work.user, "Message note owner mismatch");
            if let Some(id) = crate::message_notes::existing(&db, work)? {
                return Ok(id);
            }
        }
        if let ResourceWrite::DocumentSummary(work, source) = mode {
            anyhow::ensure!(user == work.user, "Document summary owner mismatch");
            if let Some(existing) = crate::document_summary::existing(&db, work, source)? {
                return Ok(existing);
            }
        }
        if let ResourceWrite::Profile(work, base) = mode {
            crate::profiles::authenticated(&db, work)?;
            let current: Option<String> = db
                .query_row(
                    "SELECT resource FROM participant_profiles WHERE user_id=?",
                    [user],
                    |r| r.get(0),
                )
                .optional()?;
            if current.as_deref() != base {
                bail!("Profile changed; read_my_profile before saving again");
            }
        }
        if let ResourceWrite::Revision(base) = mode {
            if db.query_row("SELECT EXISTS(SELECT 1 FROM participant_profiles p JOIN note_revisions current ON current.resource=p.resource JOIN note_revisions edited ON edited.root=current.root WHERE edited.resource=?)",[base],|r|r.get::<_,bool>(0))? {
                bail!("Use save_my_profile for directory profile revisions")
            }
        }
        let revision_base = match mode {
            ResourceWrite::Revision(base) | ResourceWrite::Profile(_, Some(base)) => Some(base),
            _ => None,
        };
        let revision = if let Some(base) = revision_base {
            let (root, number) = crate::revisions::base(&db, user, base)?;
            let (latest,latest_number):(String,i64)=db.query_row("SELECT resource,number FROM note_revisions WHERE root=? ORDER BY number DESC LIMIT 1",[&root],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if latest != base {
                if latest_number == number + 1 && crate::permissions::can_read(&db, user, &latest)?
                {
                    let (old_name, _, old_bytes) = self.verified_bytes(&db, &latest)?;
                    if name == old_name && bytes == old_bytes {
                        return Ok(latest);
                    }
                }
                bail!("Note changed since that revision. Read the latest accessible note before updating; no changes saved")
            }
            Some((root, number + 1, base))
        } else {
            None
        };

        if let Some(inbox) = inbox {
            let owner: String = db.query_row(
                "SELECT user_id FROM inbox WHERE id=? AND state='processing'",
                [inbox],
                |r| r.get(0),
            )?;
            if owner != user {
                bail!("Attachment owner mismatch")
            }
            if let Some(id) = db
                .query_row(
                    "SELECT resource FROM inbox_media WHERE inbox_id=? AND state='stored'",
                    [inbox],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                return Ok(id);
            }
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM inbox_media WHERE inbox_id=? AND state='pending')",
                [inbox],
                |r| r.get::<_, bool>(0),
            )? {
                bail!("Attachment not pending")
            }
        }
        let (personal,total):(i64,i64)=db.query_row("SELECT COALESCE(SUM(CASE WHEN owner=? THEN size ELSE 0 END),0),COALESCE(SUM(size),0) FROM resources",[user],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if personal + bytes.len() as i64 > 128 * 1024 * 1024
            || total + bytes.len() as i64 > 1024 * 1024 * 1024
        {
            bail!("Storage budget reached")
        }
        let file_name = if kind == "file" {
            "content.bin"
        } else {
            "content.md"
        };
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM resources WHERE owner=?",
            [user],
            |r| r.get(0),
        )?;
        if count >= 100 {
            bail!("Note limit reached")
        }
        let exists: i64 = db.query_row("SELECT COUNT(*) FROM users WHERE id=?", [user], |r| {
            r.get(0)
        })?;
        if exists != 1 {
            bail!("Unknown owner")
        }
        let id = Uuid::new_v4().to_string();
        let repo = git2::Repository::open(self.root.join("content"))?;
        let lfs_oid = if bytes.len() > 4096 {
            let hash = Self::lfs_oid(bytes);
            let dir = repo
                .path()
                .join("lfs/objects")
                .join(&hash[..2])
                .join(&hash[2..4]);
            fs::create_dir_all(&dir)?;
            let path = dir.join(&hash);
            if !path.exists() {
                let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
                f.write_all(bytes)?;
                f.sync_all()?;
                File::open(&dir)?.sync_all()?;
            }
            if Self::lfs_oid(&fs::read(dir.join(&hash))?) != hash {
                bail!("Stored LFS object is incomplete or corrupt")
            }
            Some(hash)
        } else {
            None
        };
        let stored = match &lfs_oid {
            Some(hash) => format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{hash}\nsize {}\n",
                bytes.len()
            )
            .into_bytes(),
            None => bytes.to_vec(),
        };
        let oid = repo.blob(&stored)?;
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parent_tree = parent.as_ref().map(|p| p.tree()).transpose()?;
        let mut file_tree = repo.treebuilder(None)?;
        file_tree.insert(file_name, oid, 0o100644)?;
        let file_tree_id = file_tree.write()?;
        let old_resources = parent_tree
            .as_ref()
            .and_then(|t| t.get_name("resources"))
            .and_then(|e| repo.find_tree(e.id()).ok());
        let mut resources = repo.treebuilder(old_resources.as_ref())?;
        resources.insert(&id, file_tree_id, 0o040000)?;
        let resources_id = resources.write()?;
        let mut tree = repo.treebuilder(parent_tree.as_ref())?;
        tree.insert("resources", resources_id, 0o040000)?;
        let attribute_oid = if lfs_oid.is_some() {
            let old = parent_tree
                .as_ref()
                .and_then(|t| t.get_name(".gitattributes"))
                .and_then(|e| repo.find_blob(e.id()).ok())
                .map(|b| String::from_utf8_lossy(b.content()).into_owned())
                .unwrap_or_default();
            let attributes =
                format!("{old}resources/{id}/{file_name} filter=lfs diff=lfs merge=lfs -text\n");
            let attr = repo.blob(attributes.as_bytes())?;
            tree.insert(".gitattributes", attr, 0o100644)?;
            fs::write(self.root.join("content/.gitattributes"), attributes)?;
            Some(attr)
        } else {
            None
        };
        let tree_id = tree.write()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Carbon", "carbon@localhost")?;
        let parents: Vec<_> = parent.iter().collect();
        let commit = repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "Add private resource",
            &tree,
            &parents,
        )?;
        repo.reference(
            &format!("refs/carbon/retained/{id}"),
            commit,
            false,
            "retain resource",
        )?;
        // Materialized copy is a cache, never an authorization source.
        let dir = self.root.join("content/resources").join(&id);
        fs::create_dir_all(&dir)?;
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.join(file_name))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        File::open(&dir)?.sync_all()?;
        for object in [
            Some(oid),
            Some(file_tree_id),
            Some(resources_id),
            Some(tree_id),
            Some(commit),
            attribute_oid,
        ]
        .into_iter()
        .flatten()
        {
            let hash = object.to_string();
            let object_dir = repo.path().join("objects").join(&hash[..2]);
            File::open(object_dir.join(&hash[2..]))?.sync_all()?;
            File::open(object_dir)?.sync_all()?;
        }
        File::open(repo.path().join("objects"))?.sync_all()?;
        let retained = repo.path().join("refs/carbon/retained");
        File::open(retained.join(&id))?.sync_all()?;
        File::open(retained)?.sync_all()?;
        db.execute(
            "INSERT INTO resources(id,owner,name,oid,commit_id,size,lfs_oid) VALUES(?,?,?,?,?,?,?)",
            params![
                id,
                user,
                name,
                oid.to_string(),
                commit.to_string(),
                bytes.len() as i64,
                lfs_oid
            ],
        )?;
        db.execute(
            "INSERT INTO resource_meta(resource,kind,mime) VALUES(?,?,?)",
            params![id, kind, mime],
        )?;
        if let Some(inbox) = inbox {
            db.execute(
                "UPDATE inbox_media SET resource=?,state='stored',reference=NULL WHERE inbox_id=?",
                params![id, inbox],
            )?;
            let notice=format!("Attachment stored privately. Resource UUID: {id}. Name: {name}. Declared MIME: {mime}. Use analyze_image for JPEG/PNG/WebP if needed; do not claim analysis before a successful tool result. Use analyze_document for PDFs including scans, or transcribe_audio for speech in Ogg/WAV/MP3/FLAC. Provider support is required. Video remains storage-only. Plain UTF-8 text can be read with read_note.");
            db.execute(
                "INSERT INTO messages(user_id,role,text) VALUES(?,'user',?)",
                params![user, notice],
            )?;
        }
        // Isolated editors never consume planner drafts or unrelated history.
        // Document summaries retain their exact PDF source; fresh message notes
        // retain an inbox receipt. Existing profile/revision sources stay intact.
        if kind != "file"
            && !matches!(
                mode,
                ResourceWrite::Profile(_, _)
                    | ResourceWrite::DocumentSummary(_, _)
                    | ResourceWrite::MessageNote(_)
            )
        {
            db.execute("INSERT INTO resource_sources(resource,source) SELECT ?1,resource FROM context_sources WHERE user_id=?2 AND resource<>?1",params![id,user])?;
        }
        if let ResourceWrite::DocumentSummary(work, source) = mode {
            crate::document_summary::published(&db, work, source, &id)?;
        }
        if let ResourceWrite::MessageNote(work) = mode {
            crate::message_notes::published(&db, work, &id)?;
        }
        if kind == "note" {
            if let Some((root, number, base)) = revision {
                crate::revisions::inherit_sources(&db, &id, &root)?;
                db.execute(
                    "INSERT INTO note_revisions(resource,root,previous,number) VALUES(?,?,?,?)",
                    params![id, root, base, number],
                )?;
                db.execute(
                    "INSERT INTO audit(event,subject) VALUES('note_revised',?)",
                    [json!({"actor":user,"base":base,"revision":id}).to_string()],
                )?;
            } else {
                db.execute(
                    "INSERT INTO note_revisions(resource,root,number) VALUES(?1,?1,1)",
                    [&id],
                )?;
            }
        }
        if matches!(mode, ResourceWrite::Profile(_, _)) {
            db.execute("INSERT INTO participant_profiles(user_id,resource,published) VALUES(?,?,1) ON CONFLICT(user_id) DO UPDATE SET resource=excluded.resource,published=1",params![user,id])?;
            db.execute(
                "INSERT INTO audit(event,subject) VALUES('profile_saved',?)",
                [json!({"actor":user,"resource":id}).to_string()],
            )?;
            db.execute(
                "INSERT INTO resource_search(resource,name,body) VALUES(?,?,?)",
                params![id, name, std::str::from_utf8(bytes)?],
            )?;
            db.execute("INSERT INTO search_indexed VALUES(?)", [&id])?;
            if let ResourceWrite::Profile(_, Some(old)) = mode {
                crate::profiles::invalidate_readers(&db, old)?;
            }
        }
        db.commit()?;
        Ok(id)
    }
    pub fn save_profile(
        &self,
        work: &Work,
        base: Option<&str>,
        name: &str,
        text: &str,
    ) -> Result<Value> {
        if name.trim().is_empty()
            || name.len() > 80
            || name.chars().any(char::is_control)
            || text.trim().is_empty()
            || text.len() > 4096
        {
            bail!("Profile exceeds limits")
        }
        let resource = self.save_resource(
            &work.user,
            name,
            text.as_bytes(),
            "note",
            "text/markdown",
            ResourceWrite::Profile(work, base),
        )?;
        Ok(
            json!({"resource":resource,"state":"published","self_reported":true,"notice":"Public to all registered participants in this Carbon instance, subject to retained source access. Private history is not public. No verification or contact authority is granted."}),
        )
    }
    pub fn pending_media(&self, w: &Work) -> Result<Option<crate::media::MediaRef>> {
        let db = self.db.lock().unwrap();
        let value:Option<String>=db.query_row("SELECT m.reference FROM inbox_media m JOIN inbox i ON i.id=m.inbox_id WHERE i.id=? AND i.user_id=? AND m.state='pending'",params![w.id,w.user],|r|r.get(0)).optional()?;
        value
            .map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub fn fail_media(&self, w: &Work) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE inbox_media SET state='failed',reference=NULL WHERE inbox_id=?",
            [w.id],
        )?;
        Ok(())
    }
    pub fn lfs_oid(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_ids_survive_restart_and_separate_same_sender_messages() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        s.ingest("a@test", "one", "PRIVATE_CONTENT").unwrap();
        s.log_incoming(
            "a@test",
            "one",
            "info",
            "message_received",
            Some("a@test"),
            "Queued",
        );
        let w = s.next().unwrap().unwrap();
        s.log_work(
            w.id,
            "debug",
            "llm_request",
            Some(&w.recipient),
            "Model request",
        );
        s.finish(&w, "PRIVATE_REPLY").unwrap();
        assert!(!s.ingest("a@test", "one", "PRIVATE_CONTENT").unwrap());
        s.ingest("a@test", "two", "PRIVATE_SECOND").unwrap();
        s.log_incoming(
            "a@test",
            "two",
            "info",
            "message_received",
            Some("a@test"),
            "Queued",
        );
        let before = s.activity().unwrap();
        let first = before["events"][1]["operation_id"].clone();
        let second = before["events"][0]["operation_id"].clone();
        assert_ne!(first, second);
        assert!(!first.is_null());
        assert_eq!(before["events"][2]["operation_id"], first);
        assert!(!before.to_string().contains("PRIVATE"));
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let (outbox, recipient, _) = s.claim_send().unwrap().unwrap();
        s.log_outbox(outbox, "info", "reply_sent", Some(&recipient), "Reply sent");
        assert_eq!(s.activity().unwrap()["events"][0]["operation_id"], first);
        s.log("info", "connected", None, "Connected");
        assert!(s.activity().unwrap()["events"][0]["operation_id"].is_null());
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM inbox_operations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn activity_migration_preserves_legacy_events_without_inventing_groups() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("state")).unwrap();
        let db = Connection::open(d.path().join("state/carbon.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE activity(id INTEGER PRIMARY KEY AUTOINCREMENT,created INTEGER NOT NULL DEFAULT(unixepoch()),level TEXT NOT NULL,event TEXT NOT NULL,identity TEXT,message TEXT NOT NULL); INSERT INTO activity(level,event,message) VALUES('info','old','Old event');").unwrap();
        drop(db);
        let s = Store::open(d.path()).unwrap();
        let events = s.activity().unwrap();
        assert_eq!(events["events"][0]["message"], "Old event");
        assert!(events["events"][0]["operation_id"].is_null());
        drop(s);
        assert!(Store::open(d.path()).is_ok());
    }
    #[test]
    fn activity_is_bounded_persistent_and_independent_of_audit() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        for _ in 0..1005 {
            s.log("info", "test", Some("person@test"), "Operation completed");
        }
        {
            let db = s.db.lock().unwrap();
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM activity", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1000
            );
            db.execute("INSERT INTO audit(event,subject) VALUES('keep','test')", [])
                .unwrap();
            db.execute("UPDATE activity SET created=unixepoch()-700000", [])
                .unwrap();
        }
        assert!(s.activity().unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty());
        s.log("error", "last", None, "Safe error message");
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let log = s.activity().unwrap();
        assert_eq!(log["events"].as_array().unwrap().len(), 1);
        assert_eq!(log["events"][0]["event"], "last");
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn unlink_cancels_queues_but_preserves_data_after_restart() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        s.set_model("test-model").unwrap();
        s.ingest("a@test", "1", "remember this").unwrap();
        let work = s.next().unwrap().unwrap();
        let note = s.save_note(&work.user, "note", "keep this").unwrap();
        s.finish(&work, "queued reply").unwrap();
        s.ingest("a@test", "2", "pending request").unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        s.record_whatsapp_logout("LoggedOut; on_connect=true")
            .unwrap();
        s.reset_whatsapp_session(&session).unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(
            s.setting("whatsapp_session").unwrap().as_deref(),
            Some(session.as_str())
        );
        assert_eq!(s.setting("model").unwrap().as_deref(), Some("test-model"));
        assert!(s.setting("whatsapp_logout").unwrap().is_none());
        assert!(s.next().unwrap().is_none());
        assert!(s.claim_send().unwrap().is_none());
        assert_eq!(s.read_note(&work.user, &note).unwrap(), "keep this");
        assert_eq!(s.history(&work.user).unwrap().len(), 2);
        s.ingest("a@test", "3", "new request").unwrap();
        assert!(s.next().unwrap().is_some());
    }
    #[test]
    fn isolation_and_dedup_survive_restart() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        assert!(Store::open(d.path()).is_err());
        s.ingest("alice@test", "m1", "private A").unwrap();
        assert!(!s.ingest("alice@test", "m1", "replay").unwrap());
        s.ingest("bob@test", "m1", "private B").unwrap();
        let a = s.next().unwrap().unwrap();
        let b = s.next().unwrap().unwrap();
        let id = s.save_note(&a.user, "AC note", "secret A").unwrap();
        assert!(s.read_note(&b.user, &id).is_err());
        assert!(s.read_note(&b.user, "missing").is_err());
        assert_eq!(s.notes(&b.user).unwrap(), json!([]));
        assert!(!format!("{:?}", s.history(&b.user).unwrap()).contains("private A"));
        s.finish(&a, "reply").unwrap();
        s.claim_send().unwrap().unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert!(s.claim_send().unwrap().is_none());
        assert_eq!(s.read_note(&a.user, &id).unwrap(), "secret A");
    }
    #[test]
    fn bootstrap_requires_real_distinct_identities_and_seals() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        assert!(s
            .bootstrap_admins(&["invented".into(), "other".into()])
            .is_err());
        s.ingest("a@test", "1", "hi").unwrap();
        s.ingest("b@test", "1", "hi").unwrap();
        let p = s.participants().unwrap();
        let a = p[0]["id"].as_str().unwrap().to_owned();
        let b = p[1]["id"].as_str().unwrap().to_owned();
        assert!(s.bootstrap_admins(&[a.clone(), a.clone()]).is_err());
        s.bootstrap_admins(&[a.clone(), b.clone()]).unwrap();
        assert!(s.bootstrap_admins(&[a, b]).is_err());
    }
    #[test]
    fn limits_are_durable() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        for i in 0..50 {
            s.ingest("a@test", &i.to_string(), "hi").unwrap();
        }
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert!(s.ingest("a@test", "51", "hi").is_err());
    }
    #[test]
    fn lfs_bytes_are_hashed_and_pointer_text_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        s.ingest("a@test", "1", "hello").unwrap();
        s.ingest("b@test", "1", "hi").unwrap();
        let a = s.next().unwrap().unwrap();
        let b = s.next().unwrap().unwrap();
        let text = "private".repeat(1000);
        let id = s.save_note(&a.user, "large note", &text).unwrap();
        assert_eq!(s.read_note(&a.user, &id).unwrap(), text);
        let hash = Store::lfs_oid(text.as_bytes());
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{hash}\nsize {}\n",
            text.len()
        );
        let other = s.save_note(&b.user, "pointer", &pointer).unwrap();
        assert_eq!(s.read_note(&b.user, &other).unwrap(), pointer);
        assert!(s.read_note(&b.user, &id).is_err());
        let file = d
            .path()
            .join("content/.git/lfs/objects")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(hash);
        fs::write(file, b"damaged").unwrap();
        assert!(s.read_note(&a.user, &id).is_err());
    }
}
