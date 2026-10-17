//! Durable, operation-local model checkpoints. Unknown tool effects are never replayed.
use crate::{
    permissions,
    store::{Store, Work},
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

#[derive(Debug)]
pub(crate) struct UncertainExecution;
impl std::fmt::Display for UncertainExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tool outcome uncertain; automatic replay prohibited")
    }
}
impl std::error::Error for UncertainExecution {}
#[derive(Debug)]
pub(crate) struct TemporaryModel;
impl std::fmt::Display for TemporaryModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Temporary model service failure")
    }
}
impl std::error::Error for TemporaryModel {}
pub(crate) fn temporary(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TemporaryModel>().is_some()
        || e.downcast_ref::<crate::llm::ModelBusy>().is_some()
        || e.downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_timeout() || e.is_connect() || e.is_body())
}
pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS execution_turns(inbox INTEGER PRIMARY KEY REFERENCES inbox(id),model TEXT NOT NULL,epoch INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'active',failures INTEGER NOT NULL DEFAULT 0,due INTEGER NOT NULL DEFAULT 0,expires INTEGER NOT NULL DEFAULT(unixepoch()+900));
    CREATE TABLE IF NOT EXISTS execution_rounds(inbox INTEGER NOT NULL REFERENCES execution_turns(inbox) ON DELETE CASCADE,round INTEGER NOT NULL,request TEXT NOT NULL,response TEXT,PRIMARY KEY(inbox,round));
    CREATE TABLE IF NOT EXISTS execution_tools(inbox INTEGER NOT NULL,round INTEGER NOT NULL,position INTEGER NOT NULL,call TEXT NOT NULL,result TEXT,PRIMARY KEY(inbox,round,position),FOREIGN KEY(inbox,round) REFERENCES execution_rounds(inbox,round) ON DELETE CASCADE);
    UPDATE execution_turns SET state='blocked' WHERE inbox IN(SELECT id FROM inbox WHERE state IN('done','cancelled','interrupted'));
    UPDATE execution_turns SET state='blocked' WHERE state IN('active','waiting') AND (expires<=unixepoch() OR EXISTS(SELECT 1 FROM execution_tools t WHERE t.inbox=execution_turns.inbox AND t.result IS NULL));
    UPDATE inbox SET state='pending' WHERE state='processing' AND id IN(SELECT inbox FROM execution_turns WHERE state IN('active','waiting','blocked'));
    UPDATE inbox SET state='interrupted' WHERE state='processing';
    DELETE FROM execution_turns WHERE inbox IN(SELECT id FROM inbox WHERE state IN('done','cancelled','interrupted') AND created<unixepoch()-86400);")?;
    Ok(())
}
fn room(db: &Connection, bytes: usize) -> Result<()> {
    let used:i64=db.query_row("SELECT COALESCE((SELECT sum(length(request)+COALESCE(length(response),0)) FROM execution_rounds),0)+COALESCE((SELECT sum(length(call)+COALESCE(length(result),0)) FROM execution_tools),0)",[],|r|r.get(0))?;
    ensure!(
        bytes <= 1024 * 1024 && used + bytes as i64 <= 64 * 1024 * 1024,
        "Execution journal storage limit reached"
    );
    Ok(())
}
impl Store {
    pub(crate) fn prepare_execution_restart(&self) -> Result<()> {
        self.restarting
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _authority = self.authority.lock().unwrap();
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        db.execute("UPDATE execution_turns SET state='blocked' WHERE state IN('active','waiting') AND (expires<=unixepoch() OR EXISTS(SELECT 1 FROM execution_tools t WHERE t.inbox=execution_turns.inbox AND result IS NULL))",[])?;
        db.execute("UPDATE inbox SET state='pending' WHERE state='processing' AND id IN(SELECT inbox FROM execution_turns WHERE state IN('active','waiting','blocked'))",[])?;
        db.execute("DELETE FROM work_epochs WHERE inbox_id IN(SELECT j.inbox FROM execution_turns j JOIN inbox i ON i.id=j.inbox WHERE i.state='pending')",[])?;
        db.commit()?;
        Ok(())
    }
    fn execution_check(&self, w: &Work) -> Result<()> {
        self.check_work(w)?;
        let db = self.db.lock().unwrap();
        ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM execution_turns WHERE inbox=? AND expires<=unixepoch())",[w.id],|r|r.get::<_,bool>(0))?,"Execution expired");
        let mut calls = db.prepare(
            "SELECT call,result FROM execution_tools WHERE inbox=? AND result IS NOT NULL",
        )?;
        for raw in calls.query_map([w.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (raw, result) = raw?;
            if serde_json::from_str::<Value>(&result)?["error"].is_string() {
                continue;
            }
            let call: Value = serde_json::from_str(&raw)?;
            let args: Value =
                serde_json::from_str(call["function"]["arguments"].as_str().unwrap_or("{}"))?;
            match call["function"]["name"].as_str() {
                Some("read_inquiry") => {
                    crate::inquiry_routing::inquiry(
                        &db,
                        &w.user,
                        args["task"].as_str().context("Saved inquiry missing")?,
                    )?;
                }
                Some(
                    "read_note" | "note_history" | "analyze_image" | "analyze_document"
                    | "transcribe_audio",
                ) => {
                    ensure!(
                        permissions::can_read(
                            &db,
                            &w.user,
                            args["id"].as_str().context("Saved resource missing")?
                        )?,
                        "Saved resource is no longer accessible"
                    );
                }
                _ => {}
            }
        }
        drop(calls);
        ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id WHERE i.id=? AND i.user_id=? AND i.recipient=? AND i.state='processing')",params![w.id,w.user,w.recipient],|r|r.get::<_,bool>(0))?,"Execution identity or state changed");
        Ok(())
    }
    pub(crate) fn begin_execution(&self, w: &Work, model: &str) -> Result<Option<Value>> {
        self.execution_check(w)?;
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM execution_turns WHERE inbox IN(SELECT id FROM inbox WHERE state IN('done','cancelled','interrupted') AND created<unixepoch()-86400)",[])?;
        let old: Option<(String, i64, String, bool)> = db
            .query_row(
                "SELECT model,epoch,state,expires>unixepoch() FROM execution_turns WHERE inbox=?",
                [w.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((m, e, s, fresh)) = old {
            if db.query_row(
                "SELECT EXISTS(SELECT 1 FROM execution_tools WHERE inbox=? AND result IS NULL)",
                [w.id],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(UncertainExecution.into());
            }
            ensure!(
                m == model
                    && e == permissions::epoch(&db)?
                    && fresh
                    && matches!(s.as_str(), "active" | "waiting"),
                "Saved execution is no longer safe to resume"
            );
            ensure!(
                !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM execution_tools WHERE inbox=? AND result IS NULL)",
                    [w.id],
                    |r| r.get::<_, bool>(0)
                )?,
                "Tool outcome uncertain; automatic replay prohibited"
            );
            db.execute(
                "UPDATE execution_turns SET state='active',due=0 WHERE inbox=?",
                [w.id],
            )?;
            let body: Option<String> = db
                .query_row(
                    "SELECT request FROM execution_rounds WHERE inbox=? AND round=0",
                    [w.id],
                    |r| r.get(0),
                )
                .optional()?;
            return body.map(|s| Ok(serde_json::from_str(&s)?)).transpose();
        }
        db.execute(
            "INSERT INTO execution_turns(inbox,model,epoch) VALUES(?,?,?)",
            params![w.id, model, permissions::epoch(&db)?],
        )?;
        Ok(None)
    }
    pub(crate) fn execution_response(
        &self,
        w: &Work,
        round: usize,
        body: &Value,
    ) -> Result<Option<Value>> {
        self.execution_check(w)?;
        let raw = body.to_string();
        let db = self.db.lock().unwrap();
        let old: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT request,response FROM execution_rounds WHERE inbox=? AND round=?",
                params![w.id, round as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((request, response)) = old {
            ensure!(request == raw, "Saved model request changed");
            return response.map(|s| Ok(serde_json::from_str(&s)?)).transpose();
        }
        room(&db, raw.len())?;
        db.execute(
            "INSERT INTO execution_rounds(inbox,round,request) VALUES(?,?,?)",
            params![w.id, round as i64, raw],
        )?;
        Ok(None)
    }
    pub(crate) fn save_execution_response(
        &self,
        w: &Work,
        round: usize,
        data: &Value,
    ) -> Result<()> {
        self.execution_check(w)?;
        let raw = data.to_string();
        let db = self.db.lock().unwrap();
        room(&db, raw.len())?;
        ensure!(db.execute("UPDATE execution_rounds SET response=? WHERE inbox=? AND round=? AND response IS NULL",params![raw,w.id,round as i64])?==1,"Model checkpoint conflict");
        Ok(())
    }
    pub(crate) fn execution_tool(
        &self,
        w: &Work,
        round: usize,
        index: usize,
        call: &Value,
    ) -> Result<Option<String>> {
        self.execution_check(w)?;
        let raw = call.to_string();
        let db = self.db.lock().unwrap();
        let old: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT call,result FROM execution_tools WHERE inbox=? AND round=? AND position=?",
                params![w.id, round as i64, index as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((stored, result)) = old {
            ensure!(stored == raw, "Saved tool call changed");
            return Ok(Some(
                result.context("Tool outcome uncertain; automatic replay prohibited")?,
            ));
        }
        room(&db, raw.len())?;
        db.execute(
            "INSERT INTO execution_tools(inbox,round,position,call) VALUES(?,?,?,?)",
            params![w.id, round as i64, index as i64, raw],
        )?;
        Ok(None)
    }
    pub(crate) fn save_execution_tool(
        &self,
        w: &Work,
        round: usize,
        index: usize,
        result: &str,
    ) -> Result<()> {
        self.execution_check(w)?;
        let db = self.db.lock().unwrap();
        room(&db, result.len())?;
        // Direct sharing persists its result in the same transaction as the grants.
        // Accept that exact checkpoint after commit, including crash recovery.
        let committed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM execution_tools WHERE inbox=?1 AND round=?2 AND position=?3 AND result=?4 AND (json_extract(call,'$.function.name')='share_requested_document' OR (json_extract(call,'$.function.name')='propose_change' AND EXISTS(SELECT 1 FROM admin_document_shares a WHERE a.inbox=?1 AND a.result=?4 AND json_extract(a.result,'$.everyone')=1))))",params![w.id,round as i64,index as i64,result],|r|r.get(0))?;
        if committed {
            return Ok(());
        }
        ensure!(db.execute("UPDATE execution_tools SET result=? WHERE inbox=? AND round=? AND position=? AND result IS NULL",params![result,w.id,round as i64,index as i64])?==1,"Tool checkpoint conflict");
        Ok(())
    }
    pub(crate) fn defer_execution(&self, w: &Work) -> Result<bool> {
        let _authority = self.authority.lock().unwrap();
        self.execution_check(w)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_turns WHERE inbox=? AND state='active' AND failures<3 AND expires>unixepoch()+120 AND epoch=? AND NOT EXISTS(SELECT 1 FROM execution_tools WHERE inbox=? AND result IS NULL))",params![w.id,permissions::epoch(&tx)?,w.id],|r|r.get(0))?;
        if !eligible {
            return Ok(false);
        }
        tx.execute("UPDATE execution_turns SET state='waiting',due=unixepoch()+CASE failures WHEN 0 THEN 15 WHEN 1 THEN 45 ELSE 120 END,failures=failures+1 WHERE inbox=?",[w.id])?;
        ensure!(
            tx.execute(
                "UPDATE inbox SET state='pending' WHERE id=? AND state='processing'",
                [w.id]
            )? == 1,
            "Cannot defer stale turn"
        );
        tx.execute("DELETE FROM work_epochs WHERE inbox_id=?", [w.id])?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn claim(s: &Store) -> Work {
        s.next().unwrap().unwrap()
    }
    #[test]
    fn completed_round_and_tool_survive_restart_without_duplicate_effect() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.ingest("actor@test", "one", "remember hello").unwrap();
        let w = claim(&s);
        s.begin_execution(&w, "test").unwrap();
        let body = json!({"messages":[],"tools":[],"model":"test"});
        assert!(s.execution_response(&w, 0, &body).unwrap().is_none());
        let response = json!({"choices":[{"message":{"tool_calls":[]}}]});
        s.save_execution_response(&w, 0, &response).unwrap();
        let call = json!({"id":"call-1"});
        assert!(s.execution_tool(&w, 0, 0, &call).unwrap().is_none());
        let id = s.save_note(&w.user, "Hello", "hello").unwrap();
        s.save_execution_tool(&w, 0, 0, &json!({"id":id}).to_string())
            .unwrap();
        s.prepare_execution_restart().unwrap();
        assert!(s.check_work(&w).is_err());
        assert!(s.next().unwrap().is_none());
        crate::concurrency::Job::Private(w.clone()).abandon(&s);
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        let resumed = claim(&s);
        assert_eq!(resumed.id, w.id);
        assert_eq!(
            s.begin_execution(&resumed, "test").unwrap(),
            Some(body.clone())
        );
        assert_eq!(
            s.execution_response(&resumed, 0, &body).unwrap(),
            Some(response)
        );
        assert!(s
            .execution_tool(&resumed, 0, 0, &call)
            .unwrap()
            .unwrap()
            .contains(&id));
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        s.finish(&resumed, "done").unwrap();
        assert!(s.next().unwrap().is_none());
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM messages WHERE role='user'", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn crash_with_unknown_tool_effect_blocks_replay() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.ingest("actor@test", "one", "hello").unwrap();
        let w = claim(&s);
        s.begin_execution(&w, "test").unwrap();
        s.execution_response(&w, 0, &json!({})).unwrap();
        s.execution_tool(&w, 0, 0, &json!({"id":"call"})).unwrap();
        let id = s.save_note(&w.user, "effect", "effect").unwrap();
        s.queue_file(&w, &id).unwrap();
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        let w = claim(&s);
        assert!(s.begin_execution(&w, "test").is_err());
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT state FROM file_outbox", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "cancelled"
        );
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        s.finish(&w, "Could not safely resume.").unwrap();
    }
    #[test]
    fn retries_are_durable_bounded_and_serialize_only_same_participant() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.ingest("actor@test", "one", "hello").unwrap();
        let w = s.claim_private(false).unwrap().unwrap();
        s.begin_execution(&w, "test").unwrap();
        assert!(s.defer_execution(&w).unwrap());
        s.ingest("actor@test", "two", "next").unwrap();
        s.ingest("other@test", "one", "other").unwrap();
        let other = s.claim_private(false).unwrap().unwrap();
        assert_ne!(other.user, w.user);
        s.finish(&other, "ok").unwrap();
        assert!(s.claim_private(false).unwrap().is_none());
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        assert!(s.claim_private(false).unwrap().is_none());
        for _ in 0..2 {
            s.db.lock()
                .unwrap()
                .execute("UPDATE execution_turns SET due=0 WHERE inbox=?", [w.id])
                .unwrap();
            let resumed = s.claim_private(false).unwrap().unwrap();
            assert_eq!(resumed.id, w.id);
            s.begin_execution(&resumed, "test").unwrap();
            assert!(s.defer_execution(&resumed).unwrap());
        }
        s.db.lock()
            .unwrap()
            .execute("UPDATE execution_turns SET due=0 WHERE inbox=?", [w.id])
            .unwrap();
        let w = s.claim_private(false).unwrap().unwrap();
        s.begin_execution(&w, "test").unwrap();
        assert!(!s.defer_execution(&w).unwrap());
        s.finish(&w, "busy").unwrap();
        assert!(s.claim_private(false).unwrap().is_some());
    }
    #[test]
    fn task_outreach_and_timer_survive_restart_but_never_publish_early() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.db.lock()
            .unwrap()
            .execute("INSERT INTO settings VALUES('experimental_tasks','1')", [])
            .unwrap();
        s.ingest("15550000002@s.whatsapp.net", "one", "hello")
            .unwrap();
        let peer = claim(&s);
        let profile = s
            .save_note(&peer.user, "Helper", "Available for inquiries")
            .unwrap();
        s.db.lock()
            .unwrap()
            .execute(
                "INSERT INTO participant_profiles VALUES(?,?,1)",
                params![peer.user, profile],
            )
            .unwrap();
        s.finish(&peer, "hi").unwrap();
        s.ingest(
            "15550000001@s.whatsapp.net",
            "one",
            "Contact this person and share request details",
        )
        .unwrap();
        let w = claim(&s);
        s.begin_execution(&w, "test").unwrap();
        s.execution_response(&w, 0, &json!({})).unwrap();
        let call = json!({"id":"staged-call"});
        s.execution_tool(&w, 0, 0, &call).unwrap();
        let task = s.create_task(&w, "Arrange a conversation").unwrap()["task"]
            .as_str()
            .unwrap()
            .to_string();
        s.authorize_task_contact(&w, &task, &peer.user, &w.text)
            .unwrap();
        s.open_task_channel(&w, &task, "", &peer.user).unwrap();
        s.send_task_message(&w, &task, &peer.user, "Hello, can you help?")
            .unwrap();
        s.schedule_task(&w, &task, 60, "Check progress").unwrap();
        s.save_execution_tool(&w, 0, 0, "{}").unwrap();
        assert!(s.claim_task_send().unwrap().is_none());
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        let w = claim(&s);
        s.begin_execution(&w, "test").unwrap();
        assert!(s.claim_task_send().unwrap().is_none());
        assert_eq!(s.tick_tasks(i64::MAX / 2).unwrap(), 0);
        s.finish(&w, "[WAIT]").unwrap();
        assert!(s.claim_task_send().unwrap().is_some());
        assert!(s.claim_task_send().unwrap().is_none());
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT state FROM agent_task_wakes", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "pending"
        );
    }
    #[test]
    fn revoked_inquiry_access_prevents_replay_without_global_epoch_change() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.db.lock()
            .unwrap()
            .execute("INSERT INTO settings VALUES('experimental_tasks','1')", [])
            .unwrap();
        s.ingest("15550000002@s.whatsapp.net", "hello", "hello")
            .unwrap();
        let peer = claim(&s);
        let profile = s
            .save_note(&peer.user, "Helper", "Available for inquiries")
            .unwrap();
        s.db.lock()
            .unwrap()
            .execute(
                "INSERT INTO participant_profiles VALUES(?,?,1)",
                params![peer.user, profile],
            )
            .unwrap();
        s.finish(&peer, "hi").unwrap();
        s.ingest(
            "15550000001@s.whatsapp.net",
            "contact",
            "Contact this person and share the request",
        )
        .unwrap();
        let w = claim(&s);
        let task = s.create_task(&w, "Arrange help").unwrap()["task"]
            .as_str()
            .unwrap()
            .to_string();
        s.authorize_task_contact(&w, &task, &peer.user, &w.text)
            .unwrap();
        s.open_task_channel(&w, &task, "", &peer.user).unwrap();
        s.send_task_message(&w, &task, &peer.user, "Can you help?")
            .unwrap();
        s.finish(&w, "[WAIT]").unwrap();
        s.db.lock()
            .unwrap()
            .execute("UPDATE agent_task_messages SET state='sent'", [])
            .unwrap();
        s.ingest(&peer.recipient, "followup", "Status?").unwrap();
        let w = claim(&s);
        s.begin_execution(&w, "test").unwrap();
        s.execution_response(&w, 0, &json!({})).unwrap();
        let call = json!({"id":"read","function":{"name":"read_inquiry","arguments":json!({"task":task,"before":i64::MAX}).to_string()}});
        s.execution_tool(&w, 0, 0, &call).unwrap();
        let result = s.read_inquiry(&w.user, &task, i64::MAX).unwrap();
        s.save_execution_tool(&w, 0, 0, &result.to_string())
            .unwrap();
        s.db.lock()
            .unwrap()
            .execute("UPDATE conversations SET state='closed'", [])
            .unwrap();
        assert!(s.begin_execution(&w, "test").is_err());
    }
    #[test]
    fn changed_authority_identity_model_expiry_and_cancel_deny_cached_work() {
        for reason in ["epoch", "identity", "model", "expiry", "cancel"] {
            let dir = tempfile::tempdir().unwrap();
            let s = Store::open(dir.path()).unwrap();
            s.ingest("actor@test", "one", "hello").unwrap();
            let w = claim(&s);
            s.begin_execution(&w, "test").unwrap();
            s.execution_response(&w, 0, &json!({})).unwrap();
            s.save_execution_response(&w, 0, &json!({"private":"secret"}))
                .unwrap();
            match reason {
                "epoch" => {
                    s.db.lock().unwrap().execute("UPDATE settings SET value=CAST(value AS INTEGER)+1 WHERE key='authority_epoch'",[]).unwrap();
                }
                "identity" => {
                    s.db.lock()
                        .unwrap()
                        .execute("DELETE FROM identities WHERE identity=?", [&w.recipient])
                        .unwrap();
                }
                "expiry" => {
                    s.db.lock()
                        .unwrap()
                        .execute("UPDATE execution_turns SET expires=0", [])
                        .unwrap();
                }
                "cancel" => {
                    s.db.lock()
                        .unwrap()
                        .execute("UPDATE inbox SET state='cancelled' WHERE id=?", [w.id])
                        .unwrap();
                }
                _ => {}
            }
            assert!(
                s.begin_execution(
                    &w,
                    if reason == "model" {
                        "different"
                    } else {
                        "test"
                    }
                )
                .is_err(),
                "{reason}"
            );
        }
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use axum::{
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    struct Provider {
        n: AtomicUsize,
        requests: Mutex<Vec<Value>>,
    }
    async fn model(State(p): State<Arc<Provider>>, Json(body): Json<Value>) -> Response {
        p.requests.lock().unwrap().push(body.clone());
        let n = p.n.fetch_add(1, Ordering::SeqCst);
        if n == 2 {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "temporary upstream timeout",
            )
                .into_response();
        }
        let call = match n {
            0 => Some(("save_note", json!({"name":"Greeting","text":"hello"}))),
            1 => {
                let result = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|v| v["role"] == "tool")
                    .unwrap();
                let result: Value =
                    serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
                Some(("send_file", json!({"id":result["id"]})))
            }
            3 => None,
            _ => panic!("Completed rounds were called again"),
        };
        Json(json!({"choices":[{"message":if let Some((name,args))=call {json!({"role":"assistant","tool_calls":[{"id":format!("call-{n}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})}else {json!({"role":"assistant","content":"Your note is saved and queued for delivery."})}}]})).into_response()
    }
    #[tokio::test]
    async fn actual_runtime_resumes_failed_model_round_after_restart_and_publishes_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let p = Arc::new(Provider {
            n: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(model))
            .with_state(p.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut rt = Arc::try_unwrap(crate::runtime::Runtime::open(dir.path(), 50, 500).unwrap())
            .ok()
            .unwrap();
        rt.llm = crate::llm::Llm::new(base.clone(), "test".into()).unwrap();
        rt.store
            .ingest(
                "actor@test",
                "request",
                "Save a greeting note and send me the file",
            )
            .unwrap();
        let w = rt.store.next().unwrap().unwrap();
        rt.process_message(&w, Some("test".into())).await.unwrap();
        assert_eq!(p.n.load(Ordering::SeqCst), 3);
        assert!(rt.store.claim_send().unwrap().is_none());
        assert_eq!(
            rt.store
                .db
                .lock()
                .unwrap()
                .query_row("SELECT state FROM file_outbox", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "staged"
        );
        drop(rt);
        let mut rt = Arc::try_unwrap(crate::runtime::Runtime::open(dir.path(), 50, 500).unwrap())
            .ok()
            .unwrap();
        rt.llm = crate::llm::Llm::new(base, "test".into()).unwrap();
        rt.store
            .db
            .lock()
            .unwrap()
            .execute("UPDATE execution_turns SET due=0", [])
            .unwrap();
        let resumed = rt.store.next().unwrap().unwrap();
        assert_eq!(w.id, resumed.id);
        rt.process_message(&resumed, Some("test".into()))
            .await
            .unwrap();
        assert_eq!(p.n.load(Ordering::SeqCst), 4);
        let requests = p.requests.lock().unwrap();
        assert_eq!(requests[2], requests[3]);
        drop(requests);
        let db = rt.store.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM file_outbox WHERE state='pending'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        server.abort();
    }
}
