//! Real Rust intake, persistence, model tool loop, consent controls and delivery claims.
//! Network provider and WhatsApp delivery are simulated; no administrator fixture exists.
use crate::{
    llm::Llm,
    runtime::Runtime,
    store::{Store, Work},
};
use axum::{extract::State, routing::post, Json, Router};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
fn work(s: &Store, who: &str, id: &str, text: &str) -> Work {
    s.ingest(who, id, text).unwrap();
    s.next().unwrap().unwrap()
}
fn publish(s: &Store, who: &str, id: &str) -> Work {
    let w = work(s, who, id, "Publish my profile");
    let r = s
        .save_note(
            &w.user,
            "Alex",
            "Alex. Repair services. Available for inquiries.",
        )
        .unwrap();
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO participant_profiles VALUES(?,?,1)",
            params![w.user, r],
        )
        .unwrap();
    s.finish(&w, "Published").unwrap();
    w
}
fn task(s: &Store, w: &Work) -> String {
    s.create_task(w, "Coordinate this request").unwrap()["task"]
        .as_str()
        .unwrap()
        .into()
}
#[test]
fn combined_consent_quoted_approval_no_admin_continuation_restart_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let peer = publish(&s, "recipient@test", "profile");
    let a = work(&s, "owner@test", "need", "Find someone for my request");
    let t = task(&s, &a);
    assert!(s.open_task_channel(&a, &t, "", &peer.user).is_err());
    assert_eq!(
        s.request_task_contact(&a, &t, &peer.user).unwrap()["state"],
        "pending"
    );
    s.finish(&a, "Model text must not replace the authoritative proposal")
        .unwrap();
    let notice: String =
        s.db.lock()
            .unwrap()
            .query_row("SELECT text FROM outbox WHERE inbox_id=?", [a.id], |r| {
                r.get(0)
            })
            .unwrap();
    assert!(notice.contains("Contact *recipient@test* and share information for this request?"));
    let oid: i64 =
        s.db.lock()
            .unwrap()
            .query_row("SELECT id FROM outbox WHERE inbox_id=?", [a.id], |r| {
                r.get(0)
            })
            .unwrap();
    let stanza = s.contact_notice_stanza(oid).unwrap().unwrap();
    s.sent(oid, true).unwrap();
    let bare = work(&s, &a.recipient, "bare", "approve");
    assert!(s.handle_contact_control(&bare).is_none());
    s.finish(&bare, "Reply to the request").unwrap();
    s.ingest_reply(&peer.recipient, "spoof", "approve", true, Some(&stanza))
        .unwrap();
    let spoof = s.next().unwrap().unwrap();
    assert!(s
        .handle_contact_control(&spoof)
        .unwrap()
        .contains("Only the requesting participant"));
    s.finish(&spoof, "Denied").unwrap();
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    s.ingest_reply(&a.recipient, "approval", "ApPrOvE", true, Some(&stanza))
        .unwrap();
    let approval = s.next_control().unwrap().unwrap();
    assert_eq!(
        s.handle_contact_control(&approval).unwrap(),
        "Contact and sharing approved."
    );
    assert!(s
        .handle_contact_control(&approval)
        .unwrap()
        .contains("already decided"));
    s.finish(&approval, "Approved").unwrap();
    // Owner event is durable and can open channel without requesting reverse/admin authority.
    let wake = s.next().unwrap().unwrap();
    assert!(s.task_event_context(&wake).unwrap().is_some());
    s.open_task_channel(&wake, &t, "", &peer.user).unwrap();
    s.send_task_message(&wake, &t, &peer.user, "What time is available?")
        .unwrap();
    s.finish(&wake, "[WAIT]").unwrap();
    let (send, identity, _, _) = s.claim_task_send().unwrap().unwrap();
    assert_eq!(identity, peer.recipient);
    s.task_sent(send, true).unwrap();
    let reply = work(&s, &peer.recipient, "answer", "Tomorrow");
    s.attach_task_reply(&reply, &t).unwrap();
    s.finish(&reply, "[WAIT]").unwrap();
    let event = s.next().unwrap().unwrap();
    s.finish(&event, "Got the reply").unwrap();
    let revoke = work(
        &s,
        &a.recipient,
        "revoke",
        "Stop contacting this participant",
    );
    s.revoke_task_contact(&revoke, &t, &peer.user).unwrap();
    assert!(s
        .send_task_message(&revoke, &t, &peer.user, "Must not send")
        .is_err());
    assert!(s.open_task_channel(&revoke, &t, "", &peer.user).is_err());
    s.finish(&revoke, "Stopped").unwrap();
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM proposals", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn explicit_request_files_privacy_blocking_and_scheduled_work() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let peer = publish(&s, "recipient@test", "profile");
    let outsider = publish(&s, "outsider@test", "outsider");
    let a = work(
        &s,
        "owner@test",
        "ask",
        "Contact the relevant person and share my request photos",
    );
    let t = task(&s, &a);
    assert!(s
        .authorize_task_contact(&a, &t, &peer.user, "invented approval")
        .is_err());
    s.authorize_task_contact(&a, &t, &peer.user, &a.text)
        .unwrap();
    s.open_task_channel(&a, &t, "", &peer.user).unwrap();
    let file = s
        .save_note(
            &a.user,
            "Request details",
            "Only necessary request information",
        )
        .unwrap();
    let secret = s
        .save_note(&outsider.user, "Secret", "FOREIGN_PRIVATE_CANARY")
        .unwrap();
    assert!(s.send_task_file(&a, &t, &peer.user, &secret).is_err());
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO resource_sources VALUES(?,?)",
            params![file, secret],
        )
        .unwrap();
    assert!(s.send_task_file(&a, &t, &peer.user, &file).is_err());
    s.db.lock()
        .unwrap()
        .execute("DELETE FROM resource_sources WHERE resource=?", [&file])
        .unwrap();
    let mut forged = a.clone();
    forged.user = peer.user.clone();
    assert!(s.send_task_file(&forged, &t, &peer.user, &file).is_err());
    s.send_task_file(&a, &t, &peer.user, &file).unwrap();
    s.send_task_file(&a, &t, &peer.user, &file).unwrap();
    s.send_task_message(&a, &t, &peer.user, "Please help")
        .unwrap();
    s.schedule_task(&a, &t, 60, "Check response").unwrap();
    assert!(s.claim_file().unwrap().is_none());
    s.finish(&a, "Contacting the relevant person").unwrap();
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM contact_requests", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let item = s.claim_file().unwrap().unwrap();
    assert_eq!(item.destination, peer.recipient);
    s.check_file_delivery(&item).unwrap();
    assert!(s.read_note(&peer.user, &file).is_err());
    assert!(s.read_task(&peer.user, &t).is_err());
    let block = work(&s, &peer.recipient, "block", "Block this participant");
    s.block_contact(&block, &a.user, true).unwrap();
    s.finish(&block, "Blocked").unwrap();
    assert!(s.check_file_delivery(&item).is_err());
    assert!(s.claim_task_send().unwrap().is_none());
    assert_eq!(s.tick_tasks(i64::MAX / 2).unwrap(), 0);
}
#[test]
fn preferences_forwarded_forged_internal_expired_and_failed_turn_authority() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let peer = publish(&s, "recipient@test", "profile");
    let pref = work(&s, &peer.recipient, "pref", "Disable new inquiries");
    s.set_contact_preferences(&pref, false).unwrap();
    s.finish(&pref, "Disabled").unwrap();
    let a = work(&s, "owner@test", "ask", "Contact recipient");
    let t = task(&s, &a);
    assert!(s
        .authorize_task_contact(&a, &t, &peer.user, &a.text)
        .is_err());
    s.finish(&a, "Unavailable").unwrap();
    let pref = work(&s, &peer.recipient, "pref2", "Allow new inquiries");
    s.set_contact_preferences(&pref, true).unwrap();
    s.finish(&pref, "Allowed").unwrap();
    let a = work(&s, "owner@test", "ask2", "Contact recipient");
    let t = task(&s, &a);
    let mut forged = a.clone();
    forged.user = peer.user.clone();
    assert!(s
        .authorize_task_contact(&forged, &t, &peer.user, &a.text)
        .is_err());
    s.db.lock()
        .unwrap()
        .execute("INSERT INTO blocked_controls VALUES(?)", [a.id])
        .unwrap();
    assert!(s
        .authorize_task_contact(&a, &t, &peer.user, &a.text)
        .is_err());
    s.db.lock()
        .unwrap()
        .execute("DELETE FROM blocked_controls WHERE inbox_id=?", [a.id])
        .unwrap();
    s.authorize_task_contact(&a, &t, &peer.user, &a.text)
        .unwrap();
    s.open_task_channel(&a, &t, "", &peer.user).unwrap();
    s.send_task_message(&a, &t, &peer.user, "Do not deliver failed turn")
        .unwrap();
    s.cancel_task_actions(&a).unwrap();
    s.finish(&a, "Failed").unwrap();
    assert!(s.claim_task_send().unwrap().is_none());
    // Internal events cannot manufacture consent or change incoming preferences.
    let event = work(&s, "owner@test", "internal", "Contact recipient");
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_task_events VALUES(?,?,?)",
            params![event.id, t, "synthetic-event"],
        )
        .unwrap();
    assert!(s
        .authorize_task_contact(&event, &t, &peer.user, &event.text)
        .is_err());
    assert!(s.set_contact_preferences(&event, false).is_err());
    s.finish(&event, "[WAIT]").unwrap();
    let a = work(&s, "owner@test", "ask3", "Find someone");
    let t = task(&s, &a);
    s.request_task_contact(&a, &t, &peer.user).unwrap();
    s.finish(&a, "pending").unwrap();
    let (oid,stanza):(i64,String)=s.db.lock().unwrap().query_row("SELECT o.id,r.stanza FROM contact_requests r JOIN outbox o ON o.inbox_id=r.source WHERE r.source=?",[a.id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    s.sent(oid, true).unwrap();
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE contact_requests SET expires=0 WHERE source=?",
            [a.id],
        )
        .unwrap();
    s.ingest_reply(&a.recipient, "expired", "approve", true, Some(&stanza))
        .unwrap();
    let w = s.next().unwrap().unwrap();
    assert_eq!(
        s.handle_contact_control(&w).unwrap(),
        "Contact and sharing expired."
    );
}
struct Provider {
    step: AtomicUsize,
    peer: String,
    task: Mutex<String>,
}
async fn model(State(p): State<Arc<Provider>>, Json(body): Json<Value>) -> Json<Value> {
    let n = p.step.fetch_add(1, Ordering::SeqCst);
    let last = body["messages"].as_array().unwrap().last().unwrap();
    let call = match n {
        0 => Some((
            "create_task",
            json!({"objective":"Contact this person for this request"}),
        )),
        1 => {
            let result: Value = serde_json::from_str(last["content"].as_str().unwrap()).unwrap();
            *p.task.lock().unwrap() = result["task"].as_str().unwrap().into();
            Some((
                "authorize_task_contact",
                json!({"task":*p.task.lock().unwrap(),"peer":p.peer,"evidence":"Contact the relevant person and share my request information"}),
            ))
        }
        2 => {
            assert!(!last["content"].as_str().unwrap().contains("error"));
            Some((
                "open_task_channel",
                json!({"task":*p.task.lock().unwrap(),"peer":p.peer,"scope":""}),
            ))
        }
        3 => {
            assert!(!last["content"].as_str().unwrap().contains("error"));
            Some((
                "send_task_message",
                json!({"task":*p.task.lock().unwrap(),"peer":p.peer,"text":"Can you help with this request?"}),
            ))
        }
        _ => {
            assert!(!last["content"].as_str().unwrap().contains("error"));
            None
        }
    };
    Json(match call {
        Some((name, args)) => {
            json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":format!("call-{n}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]})
        }
        None => {
            json!({"choices":[{"message":{"role":"assistant","content":"Your inquiry is queued."}}]})
        }
    })
}
#[tokio::test]
async fn real_runtime_model_tool_loop_first_contact_without_admin_or_resource_upgrade_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let peer = publish(&s, "recipient@test", "profile");
    drop(s);
    let p = Arc::new(Provider {
        step: AtomicUsize::new(0),
        peer: peer.user,
        task: Mutex::new(String::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm = Llm::new(
        format!("http://{}/v1", listener.local_addr().unwrap()),
        "test".into(),
    )
    .unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(model))
        .with_state(p.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let rt = Runtime::open_with_llm(dir.path(), 50, 500, llm).unwrap();
    rt.store
        .db
        .lock()
        .unwrap()
        .execute("INSERT INTO settings VALUES('experimental_tasks','1')", [])
        .unwrap();
    let w = work(
        &rt.store,
        "owner@test",
        "request",
        "Contact the relevant person and share my request information",
    );
    rt.process_message(&w, Some("mock-model".into()))
        .await
        .unwrap();
    assert_eq!(p.step.load(Ordering::SeqCst), 5);
    let delivery = rt.store.claim_task_send().unwrap().unwrap();
    assert_eq!(delivery.1, peer.recipient);
    assert_eq!(delivery.2, "Can you help with this request?");
    assert_eq!(
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM coordination_scopes", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}
#[test]
fn legacy_database_upgrade_preserves_channels_files_and_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let peer = publish(&s, "recipient@test", "profile");
    let a = work(&s, "owner@test", "legacy", "Contact recipient");
    {
        let db = s.db.lock().unwrap();
        db.execute(
            "INSERT INTO access_groups(id,name,owner) VALUES('senders','Senders',?)",
            [&a.user],
        )
        .unwrap();
        db.execute(
            "INSERT INTO access_groups(id,name,owner) VALUES('recipients','Recipients',?)",
            [&a.user],
        )
        .unwrap();
        db.execute("INSERT INTO memberships VALUES('senders',?)", [&a.user])
            .unwrap();
        db.execute(
            "INSERT INTO memberships VALUES('recipients',?)",
            [&peer.user],
        )
        .unwrap();
        db.execute("INSERT INTO coordination_scopes(id,requesters,recipients,resources) VALUES('legacy','senders','recipients',1)",[]).unwrap();
    }
    let c = s.start_conversation(&a, "legacy", &peer.user).unwrap()["conversation"]
        .as_str()
        .unwrap()
        .to_string();
    let f = s.save_note(&a.user, "Own file", "Own contents").unwrap();
    s.queue_file(&a, &f).unwrap();
    s.finish(&a, "Legacy queued").unwrap();
    // Reconstruct the pre-upgrade schemas with populated child tables.
    s.db.lock().unwrap().execute_batch("PRAGMA foreign_keys=OFF;
    CREATE TABLE old_conversations(id TEXT PRIMARY KEY,scope TEXT NOT NULL REFERENCES coordination_scopes(id),a TEXT NOT NULL REFERENCES users(id),b TEXT NOT NULL REFERENCES users(id),a_identity TEXT NOT NULL,b_identity TEXT NOT NULL,scope_revision INTEGER NOT NULL,a_generation INTEGER NOT NULL,b_generation INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'open',created INTEGER NOT NULL DEFAULT(unixepoch()),expires INTEGER NOT NULL DEFAULT(unixepoch()+86400));
    INSERT INTO old_conversations SELECT * FROM conversations; DROP TABLE conversations; ALTER TABLE old_conversations RENAME TO conversations;
    CREATE TABLE old_files(id INTEGER PRIMARY KEY,inbox_id INTEGER NOT NULL REFERENCES inbox(id),resource TEXT NOT NULL REFERENCES resources(id),size INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'staged',created INTEGER NOT NULL DEFAULT(unixepoch()),UNIQUE(inbox_id,resource));
    INSERT INTO old_files SELECT id,inbox_id,resource,size,state,created FROM file_outbox; DROP TABLE file_outbox; ALTER TABLE old_files RENAME TO file_outbox;
    PRAGMA foreign_keys=ON;").unwrap();
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    assert!(crate::coordination::valid(&s.db.lock().unwrap(), &c).unwrap());
    let file = s.claim_file().unwrap().unwrap();
    assert_eq!(file.resource, f);
    assert_eq!(file.destination, a.recipient);
    assert!(file.task.is_none());
    let db = s.db.lock().unwrap();
    assert!(db
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query([])
        .unwrap()
        .next()
        .unwrap()
        .is_none());
    assert_eq!(
        db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
#[ignore = "paid local-proxy contact consent acceptance; requires CARBON_E2E_LIVE=1"]
async fn live_model_first_contact_no_admin_consent_acceptance() {
    assert_eq!(std::env::var("CARBON_E2E_LIVE").as_deref(), Ok("1"));
    let upstream =
        std::env::var("CARBON_E2E_PROXY_URL").unwrap_or_else(|_| "http://127.0.0.1:9080/v1".into());
    let url = reqwest::Url::parse(&upstream).unwrap();
    assert!(matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    let model = std::env::var("CARBON_E2E_MODEL").unwrap_or_else(|_| "xai/grok-4.7".into());
    let artifacts = std::path::PathBuf::from(
        std::env::var("CARBON_E2E_ARTIFACT_DIR").expect("fresh artifact directory required"),
    );
    assert!(!artifacts.join("report.json").exists());
    std::fs::create_dir_all(&artifacts).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let peer = publish(&s, "15550000002@s.whatsapp.net", "profile");
    drop(s);
    let rt = Runtime::open_with_llm(
        dir.path(),
        50,
        500,
        Llm::new(upstream, "test".into()).unwrap(),
    )
    .unwrap();
    rt.store.db.lock().unwrap().execute_batch("INSERT INTO settings VALUES('experimental_tasks','1');INSERT INTO settings VALUES('debug_trace','1');").unwrap();
    let request="Find repair help among the published profiles. Contact the relevant person and share information for this request: a cabinet door hinge is broken; ask about availability. You may send the relevant request details. Do not book anything.";
    let w = work(&rt.store, "15550000001@s.whatsapp.net", "ask", request);
    rt.process_message(&w, Some(model.clone())).await.unwrap();
    let (operation, reply, tasks, grants, requests, scopes): (String, String, i64, i64, i64, i64) = {
        let db = rt.store.db.lock().unwrap();
        (
            db.query_row(
                "SELECT operation_id FROM inbox_operations WHERE inbox_id=?",
                [w.id],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row("SELECT text FROM outbox WHERE inbox_id=?", [w.id], |r| {
                r.get(0)
            })
            .unwrap(),
            db.query_row(
                "SELECT COUNT(*) FROM agent_tasks WHERE owner=?",
                [&w.user],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row(
                "SELECT COUNT(*) FROM task_contact_consents WHERE peer=? AND state='active'",
                [&peer.user],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row("SELECT COUNT(*) FROM contact_requests", [], |r| r.get(0))
                .unwrap(),
            db.query_row("SELECT COUNT(*) FROM coordination_scopes", [], |r| r.get(0))
                .unwrap(),
        )
    };
    let trace = rt.store.trace_export(&operation).unwrap();
    std::fs::write(
        artifacts.join("operation-trace.json"),
        serde_json::to_vec_pretty(&trace).unwrap(),
    )
    .unwrap();
    let delivery = rt.store.claim_task_send().unwrap();
    let sent = delivery
        .as_ref()
        .is_some_and(|d| d.1 == peer.recipient && !d.2.is_empty());
    let passed = tasks >= 1 && grants == 1 && requests == 0 && scopes == 0 && sent;
    std::fs::write(artifacts.join("report.json"),serde_json::to_vec_pretty(&json!({"model":model,"passed":passed,"request":request,"reply":reply,"tasks":tasks,"active_grants":grants,"extra_consent_requests":requests,"admin_scopes":scopes,"outreach":delivery,"limitations":["Only first-contact acceptance, not full negotiation journey","WhatsApp delivery simulated","Natural-language authority interpretation remains model judgment"]})).unwrap()).unwrap();
    assert!(
        passed,
        "Live model did not complete no-admin first contact; inspect report and trace"
    );
}
