//! Participant-owned, task-bound contact consent. No administrator delegates this authority.
use crate::{
    coordination, permissions,
    store::{Store, Work},
};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn migrate(db: &Connection) -> Result<()> {
    // SQLite's documented create/copy/drop/rename migration preserves child FK names.
    let old_scope: bool = db
        .prepare("PRAGMA table_info(conversations)")?
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, bool>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|(name, required)| name == "scope" && *required);
    let old_files = !db
        .prepare("PRAGMA table_info(file_outbox)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "peer");
    if old_scope || old_files {
        db.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let result = (|| -> Result<()> {
            let tx = db.unchecked_transaction()?;
            if old_scope {
                tx.execute_batch("CREATE TABLE conversations_new(id TEXT PRIMARY KEY,scope TEXT REFERENCES coordination_scopes(id),a TEXT NOT NULL REFERENCES users(id),b TEXT NOT NULL REFERENCES users(id),a_identity TEXT NOT NULL,b_identity TEXT NOT NULL,scope_revision INTEGER NOT NULL,a_generation INTEGER NOT NULL,b_generation INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'open',created INTEGER NOT NULL DEFAULT(unixepoch()),expires INTEGER NOT NULL DEFAULT(unixepoch()+86400));
                INSERT INTO conversations_new SELECT * FROM conversations;
                DROP TABLE conversations;
                ALTER TABLE conversations_new RENAME TO conversations;
                CREATE INDEX conversation_created ON conversations(created);")?;
            }
            if old_files {
                tx.execute_batch("CREATE TABLE file_outbox_new(id INTEGER PRIMARY KEY,inbox_id INTEGER NOT NULL REFERENCES inbox(id),resource TEXT NOT NULL REFERENCES resources(id),size INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'staged',created INTEGER NOT NULL DEFAULT(unixepoch()),task TEXT REFERENCES agent_tasks(id),peer TEXT REFERENCES users(id));
                INSERT INTO file_outbox_new(id,inbox_id,resource,size,state,created) SELECT id,inbox_id,resource,size,state,created FROM file_outbox;
                DROP TABLE file_outbox;
                ALTER TABLE file_outbox_new RENAME TO file_outbox;
                CREATE INDEX file_outbox_state ON file_outbox(state,id);
                CREATE INDEX file_outbox_created ON file_outbox(created);
                CREATE UNIQUE INDEX file_outbox_destination ON file_outbox(inbox_id,resource,COALESCE(peer,''));")?;
            }
            ensure!(
                !tx.prepare("PRAGMA foreign_key_check")?
                    .query([])?
                    .next()?
                    .is_some(),
                "Contact migration foreign key check failed"
            );
            tx.commit()?;
            Ok(())
        })();
        db.execute_batch("PRAGMA foreign_keys=ON;")?;
        result?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS contact_preferences(user_id TEXT PRIMARY KEY REFERENCES users(id),allow_new INTEGER NOT NULL DEFAULT 1 CHECK(allow_new IN(0,1)));
    CREATE TABLE IF NOT EXISTS contact_blocks(owner TEXT NOT NULL REFERENCES users(id),peer TEXT NOT NULL REFERENCES users(id),PRIMARY KEY(owner,peer));
    CREATE TABLE IF NOT EXISTS task_contact_consents(task TEXT NOT NULL REFERENCES agent_tasks(id),peer TEXT NOT NULL REFERENCES users(id),source INTEGER NOT NULL REFERENCES inbox(id),evidence TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('staged','active','revoked')),PRIMARY KEY(task,peer));
    CREATE TABLE IF NOT EXISTS contact_requests(id TEXT PRIMARY KEY,task TEXT NOT NULL REFERENCES agent_tasks(id),peer TEXT NOT NULL REFERENCES users(id),owner TEXT NOT NULL REFERENCES users(id),owner_identity TEXT NOT NULL,peer_identity TEXT NOT NULL,source INTEGER NOT NULL UNIQUE REFERENCES inbox(id),stanza TEXT NOT NULL UNIQUE,summary TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'staged',expires INTEGER NOT NULL DEFAULT(unixepoch()+86400));
    UPDATE contact_requests SET state='cancelled' WHERE state='staged' AND source NOT IN(SELECT inbox FROM execution_turns WHERE state IN('active','waiting'));
    UPDATE task_contact_consents SET state='revoked' WHERE state='staged' AND source IN(SELECT id FROM inbox WHERE state IN('interrupted','cancelled'));")?;
    Ok(())
}
pub(crate) fn verified(db: &Connection, user: &str) -> Result<String> {
    ensure!(
        db.query_row(
            "SELECT COUNT(*)=1 FROM identities WHERE user_id=?",
            [user],
            |r| r.get::<_, bool>(0)
        )?,
        "Recipient unavailable"
    );
    Ok(db.query_row(
        "SELECT identity FROM identities WHERE user_id=?",
        [user],
        |r| r.get(0),
    )?)
}
pub(crate) fn allowed(db: &Connection, sender: &str, peer: &str, new: bool) -> Result<bool> {
    Ok(sender!=peer && verified(db,sender).is_ok() && verified(db,peer).is_ok() && !db.query_row("SELECT EXISTS(SELECT 1 FROM contact_blocks WHERE (owner=?1 AND peer=?2) OR (owner=?2 AND peer=?1))",params![sender,peer],|r|r.get::<_,bool>(0))? && (!new || db.query_row("SELECT COALESCE((SELECT allow_new FROM contact_preferences WHERE user_id=?),1)",[peer],|r|r.get::<_,bool>(0))?))
}
pub(crate) fn valid(db: &Connection, id: &str) -> Result<bool> {
    contact_access(db, id, true)
}
pub(crate) fn inquiry_access(db: &Connection, id: &str) -> Result<bool> {
    contact_access(db, id, false)
}
fn contact_access(db: &Connection, id: &str, require_open: bool) -> Result<bool> {
    let row: Option<(String,String,String,String)> = db.query_row("SELECT c.a,c.b,c.a_identity,c.b_identity FROM conversations c JOIN agent_task_channels tc ON tc.conversation=c.id JOIN agent_tasks t ON t.id=tc.task AND t.owner=c.a JOIN task_contact_consents g ON g.task=tc.task AND g.peer=tc.peer WHERE c.id=? AND c.scope IS NULL AND c.state='open' AND c.expires>unixepoch() AND (?2=0 OR t.state='open') AND g.state IN('active','staged') AND (g.state='active' OR EXISTS(SELECT 1 FROM inbox i WHERE i.id=g.source AND i.state='processing'))",params![id,require_open],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((a, b, ai, bi)) = row else {
        return Ok(false);
    };
    Ok(allowed(db, &a, &b, false)? && verified(db, &a)? == ai && verified(db, &b)? == bi)
}
fn authentic(db: &Connection, w: &Work) -> Result<()> {
    ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id WHERE i.id=?1 AND i.user_id=?2 AND i.recipient=?3 AND i.text=?4 AND i.state='processing' AND i.created>unixepoch()-86400 AND NOT EXISTS(SELECT 1 FROM blocked_controls WHERE inbox_id=i.id) AND NOT EXISTS(SELECT 1 FROM agent_task_events WHERE inbox=i.id) AND NOT EXISTS(SELECT 1 FROM inbox_media WHERE inbox_id=i.id))",params![w.id,w.user,w.recipient,w.text],|r|r.get::<_,bool>(0))?,"Authenticated participant text required for contact consent");
    ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM inquiry_clarifications WHERE inbox=? AND state IN('staged','pending','resolving'))",[w.id],|r|r.get::<_,bool>(0))?,"Resolve inquiry clarification before granting contact");
    Ok(())
}
fn target(db: &Connection, w: &Work, task: &str, peer: &str) -> Result<()> {
    ensure!(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE id=? AND owner=? AND state='open')",
            params![task, w.user],
            |r| r.get::<_, bool>(0)
        )?,
        "Task unavailable"
    );
    ensure!(allowed(db, &w.user, peer, true)?, "Recipient unavailable");
    // Only verified discovery or an existing authorized destination; no invented/private directory IDs.
    ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM participant_profiles WHERE user_id=? AND published=1) OR EXISTS(SELECT 1 FROM coordination_scopes s JOIN memberships a ON a.group_id=s.requesters JOIN memberships b ON b.group_id=s.recipients WHERE s.enabled=1 AND a.user_id=? AND b.user_id=?)",params![peer,w.user,peer],|r|r.get::<_,bool>(0))?,"Recipient unavailable");
    Ok(())
}
fn grant(
    db: &Connection,
    w: &Work,
    task: &str,
    peer: &str,
    evidence: &str,
    state: &str,
) -> Result<()> {
    target(db, w, task, peer)?;
    let old: Option<String> = db
        .query_row(
            "SELECT state FROM task_contact_consents WHERE task=? AND peer=?",
            params![task, peer],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        old.as_deref() != Some("revoked"),
        "Contact consent revoked; create a new request"
    );
    db.execute(
        "INSERT OR IGNORE INTO task_contact_consents VALUES(?,?,?,?,?)",
        params![task, peer, w.id, evidence, state],
    )?;
    db.execute(
        "INSERT INTO audit(event,subject) VALUES('contact_consent_recorded',?)",
        [json!({"task":task,"peer":peer,"source":w.id,"state":state}).to_string()],
    )?;
    Ok(())
}
impl Store {
    pub fn authorize_task_contact(
        &self,
        w: &Work,
        task: &str,
        peer: &str,
        evidence: &str,
    ) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authentic(&db, w)?;
        ensure!(
            evidence.trim().len() >= 3 && evidence.len() <= 2000 && w.text.contains(evidence),
            "Contact evidence must quote the current participant request"
        );
        grant(&db, w, task, peer, evidence, "staged")?;
        db.commit()?;
        Ok(
            json!({"state":"authorized","notice":"Task contact and relevant information sharing authorized from this participant request; no admin approval. Interpretation of natural-language authority remains model judgment."}),
        )
    }
    pub fn request_task_contact(&self, w: &Work, task: &str, peer: &str) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authentic(&db, w)?;
        target(&db, w, task, peer)?;
        if db.query_row("SELECT EXISTS(SELECT 1 FROM task_contact_consents WHERE task=? AND peer=? AND state IN('active','staged'))",params![task,peer],|r|r.get::<_,bool>(0))? {return Ok(json!({"state":"authorized"}));}
        if db.query_row("SELECT EXISTS(SELECT 1 FROM contact_requests WHERE task=? AND peer=? AND owner=? AND state='pending' AND expires>unixepoch())",params![task,peer,w.user],|r|r.get::<_,bool>(0))? {
            return Ok(json!({"state":"pending","notice":"An existing combined contact-and-sharing request is awaiting your quoted approval; do not ask again."}));
        }
        let name:String=db.query_row("SELECT COALESCE((SELECT label FROM participant_labels WHERE identity=i.identity),i.identity) FROM identities i WHERE i.user_id=?",[peer],|r|r.get(0))?;
        let name: String = name
            .chars()
            .filter(|c| !c.is_control() && !['*', '\n', '\r'].contains(c))
            .take(80)
            .collect();
        let summary=format!("Contact *{name}* and share information for this request?\nReply to this message with approve or reject.\nExpires in 24h.");
        db.execute("INSERT OR IGNORE INTO contact_requests(id,task,peer,owner,owner_identity,peer_identity,source,stanza,summary) VALUES(?,?,?,?,?,?,?,?,?)",params![Uuid::new_v4().to_string(),task,peer,w.user,w.recipient,verified(&db,peer)?,w.id,Uuid::new_v4().simple().to_string(),summary])?;
        let (saved_task, saved_peer): (String, String) = db.query_row(
            "SELECT task,peer FROM contact_requests WHERE source=?",
            [w.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            saved_task == task && saved_peer == peer,
            "One contact consent request per turn"
        );
        db.commit()?;
        Ok(
            json!({"state":"pending","notice":"Carbon will send the canonical contact-and-sharing consent request when this turn succeeds. Do not open or send yet. No admin review."}),
        )
    }
    pub fn handle_contact_control(&self, w: &Work) -> Option<String> {
        let verb = w.text.trim().to_ascii_lowercase();
        if !["approve", "approving", "reject", "rejecting"].contains(&verb.as_str()) {
            return None;
        }
        let bound=self.db.lock().unwrap().query_row("SELECT r.id FROM approval_quotes q JOIN contact_requests r ON r.stanza=q.stanza_id WHERE q.inbox_id=?",[w.id],|r|r.get::<_,String>(0)).optional().ok().flatten()?;
        Some(
            self.decide_contact(w, &bound, verb.starts_with("approv"))
                .unwrap_or_else(|e| format!("Carbon: {e}")),
        )
    }
    fn decide_contact(&self, w: &Work, id: &str, approve: bool) -> Result<String> {
        let _authority = self.authority.lock().unwrap();
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authentic(&db, w)?;
        let (task,peer,owner,oi,pi,state,expired,source):(String,String,String,String,String,String,bool,i64)=db.query_row("SELECT task,peer,owner,owner_identity,peer_identity,state,expires<=unixepoch(),source FROM contact_requests WHERE id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?;
        ensure!(
            owner == w.user && oi == w.recipient,
            "Only the requesting participant can approve contact"
        );
        ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE inbox_id=? AND state IN('sent','unknown'))",[source],|r|r.get::<_,bool>(0))?,"Contact request was not delivered");
        ensure!(state == "pending", "Contact request already decided");
        let next = if expired {
            "expired"
        } else if !approve {
            "rejected"
        } else if verified(&db, &peer)? != pi || target(&db, w, &task, &peer).is_err() {
            "stale"
        } else {
            grant(&db, w, &task, &peer, "request-bound approve", "active")?;
            "approved"
        };
        db.execute(
            "UPDATE contact_requests SET state=? WHERE id=?",
            params![next, id],
        )?;
        if next == "approved" {
            crate::tasks::wake_owner(
                &db,
                &task,
                &format!("contact-{id}"),
                self.daily_user_limit,
                self.daily_global_limit,
            )?;
        }
        db.execute(
            "INSERT INTO audit(event,subject) VALUES('contact_consent_decided',?)",
            [json!({"request":id,"actor":w.user,"state":next}).to_string()],
        )?;
        db.commit()?;
        Ok(format!("Contact and sharing {next}."))
    }
    pub fn contact_notice_stanza(&self, outbox: i64) -> Result<Option<String>> {
        Ok(self.db.lock().unwrap().query_row("SELECT r.stanza FROM contact_requests r JOIN outbox o ON o.inbox_id=r.source WHERE o.id=? AND r.state='pending'",[outbox],|r|r.get(0)).optional()?)
    }
    pub fn open_contact_channel(&self, w: &Work, task: &str, peer: &str) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        ensure!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE id=? AND owner=? AND state='open')",
                params![task, w.user],
                |r| r.get::<_, bool>(0)
            )?,
            "Task unavailable"
        );
        if let Some(c) = db
            .query_row(
                "SELECT conversation FROM agent_task_channels WHERE task=? AND peer=?",
                params![task, peer],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            ensure!(
                coordination::valid(&db, &c)?,
                "Contact consent revoked; create a new request"
            );
            return Ok(json!({"state":"open"}));
        }
        ensure!(allowed(&db, &w.user, peer, true)?, "Recipient unavailable");
        ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM task_contact_consents g JOIN inbox i ON i.id=g.source WHERE g.task=? AND g.peer=? AND (g.state='active' OR (g.state='staged' AND i.id=? AND i.state='processing')))",params![task,peer,w.id],|r|r.get::<_,bool>(0))?,"Contact needs your consent");
        let (personal,total):(i64,i64)=db.query_row("SELECT COALESCE(SUM(a=?),0),COUNT(*) FROM conversations WHERE created>unixepoch()-86400",[&w.user],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            personal < 20 && total < 200,
            "Conversation start limit reached"
        );
        let c = Uuid::new_v4().to_string();
        db.execute("INSERT INTO conversations(id,scope,a,b,a_identity,b_identity,scope_revision,a_generation,b_generation,expires) VALUES(?,NULL,?,?,?,?,0,0,0,unixepoch()+2592000)",params![c,w.user,peer,verified(&db,&w.user)?,verified(&db,peer)?])?;
        db.execute(
            "INSERT INTO agent_task_channels VALUES(?,?,?)",
            params![task, c, peer],
        )?;
        db.commit()?;
        Ok(
            json!({"state":"open","notice":"No message sent. Task consent includes relevant information sharing; no resource upgrade or admin approval."}),
        )
    }
    pub fn revoke_task_contact(&self, w: &Work, task: &str, peer: &str) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authentic(&db, w)?;
        ensure!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE id=? AND owner=?)",
                params![task, w.user],
                |r| r.get::<_, bool>(0)
            )?,
            "Task unavailable"
        );
        db.execute(
            "UPDATE task_contact_consents SET state='revoked' WHERE task=? AND peer=?",
            params![task, peer],
        )?;
        db.execute("UPDATE conversations SET state='closed' WHERE id IN(SELECT conversation FROM agent_task_channels WHERE task=? AND peer=?)",params![task,peer])?;
        db.execute("UPDATE contact_requests SET state='cancelled' WHERE task=? AND peer=? AND state IN('pending','staged')",params![task,peer])?;
        db.commit()?;
        Ok(json!({"state":"revoked"}))
    }
    pub fn set_contact_preferences(&self, w: &Work, allow_new: bool) -> Result<Value> {
        self.check_task_work(w)?;
        let db = self.db.lock().unwrap();
        authentic(&db, w)?;
        db.execute("INSERT INTO contact_preferences VALUES(?,?) ON CONFLICT(user_id) DO UPDATE SET allow_new=excluded.allow_new",params![w.user,allow_new])?;
        Ok(
            json!({"allow_new_inquiries":allow_new,"notice":"Existing inquiries remain available; block a participant or STOP to close them."}),
        )
    }
    pub fn block_contact(&self, w: &Work, peer: &str, blocked: bool) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authentic(&db, w)?;
        verified(&db, peer)?;
        ensure!(peer != w.user, "Cannot block yourself");
        if blocked {
            db.execute(
                "INSERT OR IGNORE INTO contact_blocks VALUES(?,?)",
                params![w.user, peer],
            )?;
            db.execute(
                "UPDATE conversations SET state='closed' WHERE (a=?1 AND b=?2) OR (a=?2 AND b=?1)",
                params![w.user, peer],
            )?;
        } else {
            db.execute(
                "DELETE FROM contact_blocks WHERE owner=? AND peer=?",
                params![w.user, peer],
            )?;
        }
        db.commit()?;
        Ok(json!({"blocked":blocked,"notice":"Unblocking does not revive closed channels."}))
    }
    pub fn send_task_file(
        &self,
        w: &Work,
        task: &str,
        peer: &str,
        resource: &str,
    ) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        ensure!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_tasks WHERE id=? AND owner=? AND state='open')",
                params![task, w.user],
                |r| r.get::<_, bool>(0)
            )?,
            "Task unavailable"
        );
        let c: String = db.query_row(
            "SELECT conversation FROM agent_task_channels WHERE task=? AND peer=?",
            params![task, peer],
            |r| r.get(0),
        )?;
        ensure!(coordination::valid(&db, &c)?, "Recipient unavailable");
        ensure!(
            shareable(&db, &w.user, peer, resource)?,
            "Resource unavailable to this recipient"
        );
        let (size,kind):(i64,String)=db.query_row("SELECT r.size,m.kind FROM resources r JOIN resource_meta m ON m.resource=r.id WHERE r.id=?",[resource],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(kind != "folder", "Not a sendable file");
        if let Some(state) = db
            .query_row(
                "SELECT state FROM file_outbox WHERE inbox_id=? AND resource=? AND peer=?",
                params![w.id, resource, peer],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(json!({"state":state}));
        }
        ensure!(db.query_row("SELECT (SELECT COUNT(*) FROM file_outbox WHERE inbox_id=?1)+(SELECT COUNT(*) FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task WHERE t.inbox_id=?1)",[w.id],|r|r.get::<_,i64>(0))?<3,"At most three files per request");
        crate::scoped_resources::outbound_budget(&db, &w.user, size)?;
        db.execute(
            "INSERT OR IGNORE INTO file_outbox(inbox_id,resource,size,task,peer,stanza) VALUES(?,?,?,?,?,?)",
            params![w.id, resource, size, task, peer, Uuid::new_v4().simple().to_string()],
        )?;
        db.commit()?;
        Ok(
            json!({"state":"staged","notice":"Only the selected file is staged for this task recipient; no general read access granted. Delivery pending."}),
        )
    }
}
pub(crate) fn shareable(db: &Connection, owner: &str, peer: &str, resource: &str) -> Result<bool> {
    Ok(permissions::can_read(db,owner,resource)? && db.query_row("SELECT owner=? FROM resources WHERE id=?",params![owner,resource],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false) && !db.query_row("WITH RECURSIVE sources(id) AS (SELECT source FROM resource_sources WHERE resource=? UNION SELECT s.source FROM resource_sources s JOIN sources p ON s.resource=p.id) SELECT EXISTS(SELECT 1 FROM sources s JOIN resources r ON r.id=s.id WHERE r.owner<>?)",params![resource,owner],|r|r.get::<_,bool>(0))? && verified(db,peer).is_ok())
}
