//! Task inquiry continuity: transport binding, private participant history and durable clarification.
use crate::{
    coordination, permissions,
    store::{Store, Work},
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn migrate(db: &Connection) -> Result<()> {
    let cols = db
        .prepare("PRAGMA table_info(file_outbox)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !cols.iter().any(|c| c == "stanza") {
        db.execute("ALTER TABLE file_outbox ADD COLUMN stanza TEXT", [])?;
    }
    db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS file_outbox_stanza ON file_outbox(stanza);
    CREATE TABLE IF NOT EXISTS inquiry_reply_turns(inbox INTEGER PRIMARY KEY REFERENCES inbox(id),turn INTEGER NOT NULL REFERENCES inbox(id));
    CREATE INDEX IF NOT EXISTS inquiry_reply_turn ON inquiry_reply_turns(turn);
    CREATE TABLE IF NOT EXISTS inquiry_clarifications(inbox INTEGER PRIMARY KEY REFERENCES inbox(id),participant TEXT NOT NULL REFERENCES users(id),identity TEXT NOT NULL,candidates TEXT NOT NULL,question TEXT NOT NULL,stanza TEXT NOT NULL UNIQUE,state TEXT NOT NULL DEFAULT 'staged',expires INTEGER NOT NULL DEFAULT(unixepoch()+86400),resolution_turn INTEGER REFERENCES inbox(id));
    CREATE INDEX IF NOT EXISTS inquiry_clarification_participant ON inquiry_clarifications(participant,state);
    CREATE TABLE IF NOT EXISTS inquiry_pending_wakes(task TEXT NOT NULL REFERENCES agent_tasks(id),peer TEXT NOT NULL REFERENCES users(id),event_key TEXT NOT NULL,PRIMARY KEY(task,peer));
    UPDATE inquiry_clarifications SET state='cancelled' WHERE state='staged' AND inbox NOT IN(SELECT inbox FROM execution_turns WHERE state IN('active','waiting'));
    UPDATE inquiry_clarifications SET state='pending',resolution_turn=NULL WHERE state='resolving' AND resolution_turn IN(SELECT id FROM inbox WHERE state IN('interrupted','cancelled'));
    DELETE FROM agent_task_routes WHERE inbox IN(SELECT m.source FROM agent_task_messages m JOIN inquiry_reply_turns r ON r.inbox=m.source JOIN inbox i ON i.id=r.turn WHERE m.state='staged_in' AND i.state IN('interrupted','cancelled'));
    DELETE FROM agent_task_messages WHERE state='staged_in' AND source IN(SELECT r.inbox FROM inquiry_reply_turns r JOIN inbox i ON i.id=r.turn WHERE i.state IN('interrupted','cancelled'));
    DELETE FROM inquiry_reply_turns WHERE turn IN(SELECT id FROM inbox WHERE state IN('interrupted','cancelled'));")?;
    Ok(())
}
fn authenticated(db: &Connection, w: &Work) -> Result<()> {
    ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id WHERE i.id=?1 AND i.user_id=?2 AND i.recipient=?3 AND i.text=?4 AND i.state='processing' AND i.created>unixepoch()-86400 AND NOT EXISTS(SELECT 1 FROM blocked_controls WHERE inbox_id=i.id) AND NOT EXISTS(SELECT 1 FROM agent_task_events WHERE inbox=i.id))",params![w.id,w.user,w.recipient,w.text],|r|r.get::<_,bool>(0))?,"Fresh authenticated participant reply required");
    Ok(())
}
pub(crate) fn inquiry(
    db: &Connection,
    actor: &str,
    task: &str,
) -> Result<(String, String, String, bool)> {
    let (c,owner,state):(String,String,String)=db.query_row("SELECT tc.conversation,t.owner,t.state FROM agent_task_channels tc JOIN agent_tasks t ON t.id=tc.task WHERE tc.task=? AND tc.peer=?",params![task,actor],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.context("Inquiry unavailable")?;
    let personal: bool = db.query_row(
        "SELECT scope IS NULL FROM conversations WHERE id=?",
        [&c],
        |r| r.get(0),
    )?;
    ensure!(
        if personal {
            crate::contacts::inquiry_access(db, &c)?
        } else {
            coordination::valid(db, &c)?
        },
        "Inquiry unavailable"
    );
    ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM agent_task_messages WHERE task=?1 AND peer=?2 AND direction='out' AND state='sent') OR EXISTS(SELECT 1 FROM file_outbox WHERE task=?1 AND peer=?2 AND state='sent')",params![task,actor],|r|r.get::<_,bool>(0))?,"Inquiry unavailable");
    let reply = state == "open" && coordination::valid(db, &c)?;
    Ok((c, owner, state, reply))
}
fn stage_reply(db: &Connection, w: &Work, task: &str, source: i64) -> Result<()> {
    let (_, _, _, can_reply) = inquiry(db, &w.user, task)?;
    ensure!(can_reply, "Inquiry closed; no message forwarded");
    let text:String=db.query_row("SELECT i.text FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id JOIN work_epochs e ON e.inbox_id=i.id WHERE i.id=? AND i.user_id=? AND i.state IN('processing','done') AND i.created>unixepoch()-86400 AND NOT EXISTS(SELECT 1 FROM blocked_controls WHERE inbox_id=i.id) AND NOT EXISTS(SELECT 1 FROM agent_task_events WHERE inbox=i.id)",params![source,w.user],|r|r.get(0))?;
    if let Some(previous) = db
        .query_row(
            "SELECT task FROM agent_task_routes WHERE inbox=?",
            [source],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        ensure!(previous == task, "Message already assigned");
        return Ok(());
    }
    ensure!(
        db.query_row(
            "SELECT COUNT(*) FROM agent_task_messages WHERE task=?",
            [task],
            |r| r.get::<_, i64>(0)
        )? < 100,
        "Task message budget exhausted"
    );
    db.execute(
        "INSERT INTO agent_task_routes VALUES(?,?)",
        params![source, task],
    )?;
    db.execute(
        "INSERT INTO inquiry_reply_turns VALUES(?,?)",
        params![source, w.id],
    )?;
    db.execute("INSERT INTO agent_task_messages(task,peer,direction,text,source,state,epoch,stanza) VALUES(?,?,'in',?,?,'staged_in',?,?)",params![task,w.user,text,source,permissions::epoch(db)?,Uuid::new_v4().simple().to_string()])?;
    Ok(())
}
pub enum QuoteRoute {
    Private,
    Attached,
    Unavailable,
}
impl Store {
    pub fn read_inquiry(&self, actor: &str, task: &str, before: i64) -> Result<Value> {
        ensure!(before > 0, "Positive cursor required");
        let db = self.db.lock().unwrap();
        let (c, owner, state, can_reply) = inquiry(&db, actor, task)?;
        let name:String=db.query_row("SELECT COALESCE((SELECT r.name FROM participant_profiles p JOIN resources r ON r.id=p.resource WHERE p.user_id=t.owner AND p.published=1),(SELECT l.label FROM identities i JOIN participant_labels l ON l.identity=i.identity WHERE i.user_id=t.owner),'Participant') FROM agent_tasks t WHERE t.id=?",[task],|r|r.get(0))?;
        let channel_state: String =
            db.query_row("SELECT state FROM conversations WHERE id=?", [&c], |r| {
                r.get(0)
            })?;
        let mut q=db.prepare("SELECT id,direction,text FROM agent_task_messages WHERE task=? AND peer=? AND id<? AND ((direction='out' AND state='sent') OR (direction='in' AND state='received')) ORDER BY id DESC LIMIT 31")?;
        let rows = q
            .query_map(params![task, actor, before], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = Vec::new();
        let mut bytes = 0;
        let mut truncated = false;
        for (id, direction, text) in rows {
            if messages.len() == 30 || bytes + text.len() > 32768 {
                truncated = true;
                break;
            }
            bytes += text.len();
            messages.push(json!({"id":id,"direction":direction,"text":text}));
        }
        messages.reverse();
        let next = messages.first().map(|m| m["id"].clone());
        let mut f=db.prepare("SELECT f.resource,r.name,m.mime,f.size FROM file_outbox f JOIN resources r ON r.id=f.resource JOIN resource_meta m ON m.resource=r.id WHERE f.task=? AND f.peer=? AND f.state='sent' ORDER BY f.id DESC LIMIT 20")?;
        let files=f.query_map(params![task,actor],|r|Ok(json!({"resource":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"mime":r.get::<_,String>(2)?,"size":r.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let pending: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_task_wakes WHERE task=? AND state='pending')",
            [task],
            |r| r.get(0),
        )?;
        Ok(
            json!({"task":task,"coordinator":{"participant":owner,"name":name},"task_state":state,"channel_state":channel_state,"can_reply":can_reply,"coordinator_wake_state":if pending{"scheduled"}else{"none_pending"},"notice":"Wake state is task-level, not proof of a follow-up for your channel. Private purposes and other channels are not exposed. File metadata describes delivered files, not general read permission.","messages":messages,"files":files,"history_truncated":truncated,"before_cursor":next}),
        )
    }
    pub fn stage_inquiry_reply(&self, w: &Work, task: &str) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authenticated(&db, w)?;
        ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM inquiry_clarifications WHERE inbox=? AND state IN('staged','pending','resolving'))",[w.id],|r|r.get::<_,bool>(0))?,"Resolve the pending clarification instead");
        if let Some((source,state,turn))=db.query_row("SELECT c.inbox,c.state,c.resolution_turn FROM approval_quotes q JOIN inquiry_clarifications c ON c.stanza=q.stanza_id WHERE q.inbox_id=?",[w.id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?))).optional()? {
            ensure!(state=="resolving" && turn==Some(w.id) && db.query_row("SELECT task=? FROM agent_task_routes WHERE inbox=?",params![task,source],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false),"Resolve the pending clarification instead");
        }
        stage_reply(&db, w, task, w.id)?;
        db.commit()?;
        Ok(
            json!({"state":"staged","notice":"Only this authenticated message is associated. Published to the coordinator when this turn succeeds; no separate acknowledgement needed."}),
        )
    }
    pub fn route_task_quote(&self, w: &Work) -> Result<QuoteRoute> {
        let _authority = self.authority.lock().unwrap();
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        let row:Option<(String,String,String)>=db.query_row("SELECT m.task,m.peer,m.state FROM approval_quotes q JOIN agent_task_messages m ON m.stanza=q.stanza_id AND m.direction='out' WHERE q.inbox_id=?1 UNION ALL SELECT f.task,f.peer,f.state FROM approval_quotes q JOIN file_outbox f ON f.stanza=q.stanza_id WHERE q.inbox_id=?1 AND f.task IS NOT NULL",[w.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((task, peer, state)) = row else {
            return Ok(QuoteRoute::Private);
        };
        if peer != w.user
            || !["sent", "unknown", "sending"].contains(&state.as_str())
            || authenticated(&db, w).is_err()
        {
            return Ok(QuoteRoute::Unavailable);
        }
        if state == "unknown" || state == "sending" {
            db.execute("UPDATE agent_task_messages SET state='sent' WHERE state IN('unknown','sending') AND stanza IN(SELECT stanza_id FROM approval_quotes WHERE inbox_id=?)",[w.id])?;
            db.execute("UPDATE file_outbox SET state='sent' WHERE state IN('unknown','sending') AND stanza IN(SELECT stanza_id FROM approval_quotes WHERE inbox_id=?)",[w.id])?;
        }
        if inquiry(&db, &w.user, &task).is_err() {
            return Ok(QuoteRoute::Unavailable);
        }
        if stage_reply(&db, w, &task, w.id).is_err() {
            return Ok(QuoteRoute::Unavailable);
        }
        db.commit()?;
        Ok(QuoteRoute::Attached)
    }
    pub fn ask_inquiry_clarification(
        &self,
        w: &Work,
        tasks: &[String],
        question: &str,
    ) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authenticated(&db, w)?;
        ensure!(
            (2..=5).contains(&tasks.len()) && !question.trim().is_empty() && question.len() <= 500,
            "Bounded clarification required"
        );
        ensure!(
            tasks.iter().collect::<std::collections::HashSet<_>>().len() == tasks.len(),
            "Distinct candidates required"
        );
        for t in tasks {
            ensure!(inquiry(&db, &w.user, t)?.3, "Inquiry unavailable");
        }
        ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM approval_quotes q WHERE q.inbox_id=? AND (EXISTS(SELECT 1 FROM agent_task_messages m WHERE m.stanza=q.stanza_id AND m.direction='out') OR EXISTS(SELECT 1 FROM file_outbox f WHERE f.stanza=q.stanza_id AND f.task IS NOT NULL) OR EXISTS(SELECT 1 FROM contact_requests c WHERE c.stanza=q.stanza_id) OR EXISTS(SELECT 1 FROM inquiry_clarifications c WHERE c.stanza=q.stanza_id) OR EXISTS(SELECT 1 FROM approval_notifications n WHERE n.stanza_id=q.stanza_id)))",[w.id],|r|r.get::<_,bool>(0))?,"Do not replace a quoted destination with clarification");
        ensure!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_task_routes WHERE inbox=?)",
                [w.id],
                |r| r.get::<_, bool>(0)
            )?,
            "Message already assigned"
        );
        ensure!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM contact_requests WHERE source=? AND state='staged')",
                [w.id],
                |r| r.get::<_, bool>(0)
            )?,
            "One user decision per turn"
        );
        db.execute("UPDATE inquiry_clarifications SET state='expired' WHERE participant=? AND state='pending' AND expires<=unixepoch()",[&w.user])?;
        if let Some(existing)=db.query_row("SELECT question FROM inquiry_clarifications WHERE participant=? AND state='pending'",[&w.user],|r|r.get::<_,String>(0)).optional()? {return Ok(json!({"state":"pending","question":existing,"notice":"Clarification already pending; do not create or forward another message."}));}
        db.execute("INSERT OR IGNORE INTO inquiry_clarifications(inbox,participant,identity,candidates,question,stanza) VALUES(?,?,?,?,?,?)",params![w.id,w.user,w.recipient,serde_json::to_string(tasks)?,question,Uuid::new_v4().simple().to_string()])?;
        db.commit()?;
        Ok(
            json!({"state":"staged","notice":"Carbon will send this factual clarification at successful turn completion. Original message remains unshared."}),
        )
    }
    pub fn pending_inquiry_clarification(&self, w: &Work) -> Result<Option<Value>> {
        let db = self.db.lock().unwrap();
        let row:Option<(i64,String,String,String)>=db.query_row("SELECT c.inbox,c.candidates,c.question,i.text FROM inquiry_clarifications c JOIN inbox i ON i.id=c.inbox WHERE c.participant=? AND c.identity=? AND c.state='pending' AND c.expires>unixepoch() ORDER BY c.inbox",params![w.user,w.recipient],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((source, candidates, question, text)) = row else {
            return Ok(None);
        };
        let ids: Vec<String> = serde_json::from_str(&candidates)?;
        let mut visible = Vec::new();
        for t in ids {
            if let Ok((_, owner, state, can_reply)) = inquiry(&db, &w.user, &t) {
                visible.push(
                    json!({"task":t,"coordinator":owner,"task_state":state,"can_reply":can_reply}),
                );
            }
        }
        Ok(Some(
            json!({"original_message_id":source,"original_message":text,"question":question,"candidates":visible,"notice":"Only resolve if this current message identifies an intended candidate. An unrelated private request does not resolve it."}),
        ))
    }
    pub fn resolve_inquiry_clarification(&self, w: &Work, task: &str) -> Result<Value> {
        self.check_task_work(w)?;
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        authenticated(&db, w)?;
        let (source,candidates):(i64,String)=db.query_row("SELECT inbox,candidates FROM inquiry_clarifications WHERE participant=? AND identity=? AND state='pending' AND expires>unixepoch()",params![w.user,w.recipient],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.context("No active inquiry clarification")?;
        let ids: Vec<String> = serde_json::from_str(&candidates)?;
        ensure!(
            ids.iter().any(|id| id == task),
            "Choose only a saved clarification candidate"
        );
        if let Some(quote) = db
            .query_row(
                "SELECT stanza_id FROM approval_quotes WHERE inbox_id=?",
                [w.id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            ensure!(
                db.query_row(
                    "SELECT stanza=? FROM inquiry_clarifications WHERE inbox=?",
                    params![quote, source],
                    |r| r.get::<_, bool>(0)
                )?,
                "This reply quotes a different destination"
            );
            ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE inbox_id=? AND state IN('sent','unknown'))",[source],|r|r.get::<_,bool>(0))?,"Clarification was not delivered");
        }
        ensure!(
            db.query_row("SELECT state='done' FROM inbox WHERE id=?", [source], |r| r
                .get::<_, bool>(0))?,
            "Original clarification turn incomplete"
        );
        stage_reply(&db, w, task, source)?;
        db.execute(
            "UPDATE inquiry_clarifications SET state='resolving',resolution_turn=? WHERE inbox=?",
            params![w.id, source],
        )?;
        db.commit()?;
        Ok(
            json!({"state":"staged","notice":"The saved original message alone is associated with the selected inquiry when this turn succeeds. Do not forward the selection answer unless it also contains useful new facts; attach_task_reply can then associate that current message separately. Return [WAIT]."}),
        )
    }
    pub fn cancel_inquiry_clarification(&self, w: &Work) -> Result<Value> {
        self.check_task_work(w)?;
        let db = self.db.lock().unwrap();
        authenticated(&db, w)?;
        db.execute("UPDATE inquiry_clarifications SET state='cancelled' WHERE participant=? AND state='pending'",[&w.user])?;
        Ok(json!({"state":"cancelled","notice":"Original message remains private."}))
    }
    pub fn clarification_notice_stanza(&self, outbox: i64) -> Result<Option<String>> {
        Ok(self.db.lock().unwrap().query_row("SELECT c.stanza FROM inquiry_clarifications c JOIN outbox o ON o.inbox_id=c.inbox WHERE o.id=? AND c.state='pending'",[outbox],|r|r.get(0)).optional()?)
    }
    pub fn clarification_quote_unavailable(&self, w: &Work) -> Result<bool> {
        let db = self.db.lock().unwrap();
        let row:Option<(String,String,String,bool)>=db.query_row("SELECT c.participant,c.identity,c.state,c.expires<=unixepoch() FROM approval_quotes q JOIN inquiry_clarifications c ON c.stanza=q.stanza_id WHERE q.inbox_id=?",[w.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        Ok(row.is_some_and(|(actor, identity, state, expired)| {
            actor != w.user || identity != w.recipient || state != "pending" || expired
        }))
    }
}
pub(crate) fn publish_turn(
    db: &Connection,
    w: &Work,
    user_limit: i64,
    global_limit: i64,
) -> Result<()> {
    let mut q=db.prepare("SELECT r.inbox,m.task,m.peer FROM inquiry_reply_turns r JOIN agent_task_messages m ON m.source=r.inbox AND m.direction='in' AND m.state='staged_in' WHERE r.turn=?")?;
    let rows = q
        .query_map([w.id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    for (source, task, peer) in rows {
        ensure!(
            inquiry(db, &peer, &task)?.3,
            "Inquiry closed; no message forwarded"
        );
        db.execute("UPDATE agent_task_messages SET state='received' WHERE source=? AND task=? AND direction='in' AND state='staged_in'",params![source,task])?;
        let key = format!("reply-{source}");
        if let Err(e) = crate::tasks::wake_owner(db, &task, &key, user_limit, global_limit) {
            ensure!(
                e.to_string() == "Daily autonomous work budget exhausted",
                "Cannot enqueue inquiry continuation"
            );
            db.execute(
                "INSERT OR IGNORE INTO inquiry_pending_wakes VALUES(?,?,?)",
                params![task, peer, key],
            )?;
        }
    }
    db.execute("UPDATE inquiry_clarifications SET state='resolved' WHERE state='resolving' AND resolution_turn=?",[w.id])?;
    Ok(())
}
pub(crate) fn cancel_turn(db: &Connection, turn: i64) -> Result<()> {
    db.execute("DELETE FROM agent_task_routes WHERE inbox IN(SELECT r.inbox FROM inquiry_reply_turns r JOIN agent_task_messages m ON m.source=r.inbox WHERE r.turn=? AND m.state='staged_in')",[turn])?;
    db.execute("DELETE FROM agent_task_messages WHERE state='staged_in' AND source IN(SELECT inbox FROM inquiry_reply_turns WHERE turn=?)",[turn])?;
    db.execute("DELETE FROM inquiry_reply_turns WHERE turn=?", [turn])?;
    db.execute(
        "UPDATE inquiry_clarifications SET state='cancelled' WHERE inbox=? AND state='staged'",
        [turn],
    )?;
    db.execute("UPDATE inquiry_clarifications SET state='pending',resolution_turn=NULL WHERE resolution_turn=? AND state='resolving'",[turn])?;
    Ok(())
}
pub(crate) fn retry_wakes(db: &Connection, user_limit: i64, global_limit: i64) -> Result<()> {
    let mut q = db.prepare("SELECT task,peer,event_key FROM inquiry_pending_wakes LIMIT 10")?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    for (task, peer, key) in rows {
        let permitted = inquiry(db, &peer, &task).is_ok_and(|v| v.3);
        if !permitted || crate::tasks::wake_owner(db, &task, &key, user_limit, global_limit).is_ok()
        {
            db.execute(
                "DELETE FROM inquiry_pending_wakes WHERE task=? AND peer=?",
                params![task, peer],
            )?;
        }
    }
    Ok(())
}
