//! Multiple requesters, one recipient: real Rust intake/model loop/history and durable association.
use crate::{
    llm::Llm,
    runtime::Runtime,
    store::{Store, Work},
};
use axum::{extract::State, routing::post, Json, Router};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
fn drain_events(s: &Store) {
    while let Some(w) = s.next().unwrap() {
        assert!(s.task_event_context(&w).unwrap().is_some());
        s.finish(&w, "[WAIT]").unwrap();
    }
}
fn work(s: &Store, identity: &str, text: &str) -> Work {
    drain_events(s);
    s.ingest(identity, &uuid::Uuid::new_v4().to_string(), text)
        .unwrap();
    s.next().unwrap().unwrap()
}
fn participant(s: &Store, identity: &str, name: &str) -> Work {
    let w = work(s, identity, "Publish my profile");
    let r = s
        .save_note(
            &w.user,
            name,
            &format!("{name}. Participant available for inquiries."),
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
struct Fixture {
    dir: tempfile::TempDir,
    rt: Runtime,
    a: Work,
    b: Work,
    c: Work,
    d: Work,
    ta: String,
    tb: String,
    qa: String,
    qb: String,
    file: String,
    file_quote: String,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let rt = Arc::try_unwrap(
        Runtime::open_with_llm(
            dir.path(),
            50,
            500,
            Llm::new("http://127.0.0.1:9/v1".into(), "test".into()).unwrap(),
        )
        .unwrap(),
    )
    .ok()
    .unwrap();
    let s = &rt.store;
    s.db.lock().unwrap().execute_batch("INSERT INTO settings VALUES('experimental_tasks','1');INSERT INTO settings VALUES('debug_trace','1');").unwrap();
    let a = participant(s, "15550000001@s.whatsapp.net", "Alex");
    let b = participant(s, "15550000002@s.whatsapp.net", "Casey");
    let c = participant(s, "15550000003@s.whatsapp.net", "Jordan");
    let d = participant(s, "15550000004@s.whatsapp.net", "Drew");
    let aw = work(
        s,
        &a.recipient,
        "Contact Jordan and Drew for the door repair and share relevant request information",
    );
    let ta = s
        .create_task(&aw, "Door repair. OWNER_OBJECTIVE_CANARY")
        .unwrap()["task"]
        .as_str()
        .unwrap()
        .to_string();
    s.update_task(&aw, &ta, 0, "OWNER_PRIVATE_NOTE_CANARY", "open")
        .unwrap();
    for peer in [&c.user, &d.user] {
        s.authorize_task_contact(&aw, &ta, peer, &aw.text).unwrap();
        s.open_task_channel(&aw, &ta, "", peer).unwrap();
    }
    s.send_task_message(
        &aw,
        &ta,
        &c.user,
        "Alex here: can you help with the door repair tomorrow?",
    )
    .unwrap();
    s.send_task_message(&aw, &ta, &d.user, "OTHER_RECIPIENT_TERMS_CANARY")
        .unwrap();
    let file = s
        .save_note(&a.user, "Repair description", "Door repair details")
        .unwrap();
    s.send_task_file(&aw, &ta, &c.user, &file).unwrap();
    s.schedule_task(&aw, &ta, 60, "PRIVATE_TIMER_PURPOSE_CANARY")
        .unwrap();
    s.finish(&aw, "[WAIT]").unwrap();
    let bw = work(
        s,
        &b.recipient,
        "Contact Jordan about camera installation and share relevant request information",
    );
    let tb = s
        .create_task(&bw, "Camera installation. OTHER_OWNER_OBJECTIVE_CANARY")
        .unwrap()["task"]
        .as_str()
        .unwrap()
        .to_string();
    s.update_task(&bw, &tb, 0, "OTHER_OWNER_PRIVATE_NOTE_CANARY", "open")
        .unwrap();
    s.authorize_task_contact(&bw, &tb, &c.user, &bw.text)
        .unwrap();
    s.open_task_channel(&bw, &tb, "", &c.user).unwrap();
    s.send_task_message(
        &bw,
        &tb,
        &c.user,
        "Casey here: can you help with camera installation tomorrow?",
    )
    .unwrap();
    s.finish(&bw, "[WAIT]").unwrap();
    let mut qa = String::new();
    let mut qb = String::new();
    while let Some((id, recipient, text, stanza)) = s.claim_task_send().unwrap() {
        if recipient == c.recipient {
            if text.contains("door repair") {
                qa = stanza
            } else {
                qb = stanza
            }
        }
        s.task_sent(id, true).unwrap();
    }
    let item = s.claim_file().unwrap().unwrap();
    let file_quote = item
        .stanza
        .clone()
        .expect("Task file must have a stored transport ID");
    s.file_delivery_result(item.id, "sent").unwrap();
    let cw = work(s, &c.recipient, "Finish my separate address lookup helper");
    let helper = s.create_task(&cw, "Private address lookup helper").unwrap()["task"]
        .as_str()
        .unwrap()
        .to_string();
    s.update_task(&cw, &helper, 0, "Only this helper is finished", "completed")
        .unwrap();
    s.finish(&cw, "[WAIT]").unwrap();
    Fixture {
        dir,
        rt,
        a,
        b,
        c,
        d,
        ta,
        tb,
        qa,
        qb,
        file,
        file_quote,
    }
}
fn received(s: &Store, task: &str, source: i64) -> bool {
    s.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM agent_task_messages WHERE task=? AND source=? AND direction='in' AND state='received')",params![task,source],|r|r.get(0)).unwrap()
}
fn quote(s: &Store, identity: &str, text: &str, stanza: &str, allowed: bool) -> Work {
    drain_events(s);
    s.ingest_reply(
        identity,
        &uuid::Uuid::new_v4().to_string(),
        text,
        allowed,
        Some(stanza),
    )
    .unwrap();
    s.next().unwrap().unwrap()
}
#[test]
fn history_is_participant_only_paginated_and_lifecycle_states_are_separate() {
    let f = fixture();
    let s = &f.rt.store;
    let v = s.read_inquiry(&f.c.user, &f.ta, i64::MAX).unwrap();
    let text = v.to_string();
    for secret in [
        "OWNER_OBJECTIVE_CANARY",
        "OWNER_PRIVATE_NOTE_CANARY",
        "OTHER_RECIPIENT_TERMS_CANARY",
        "PRIVATE_TIMER_PURPOSE_CANARY",
        "OTHER_OWNER_PRIVATE_NOTE_CANARY",
    ] {
        assert!(!text.contains(secret));
    }
    assert!(text.contains("door repair"));
    assert_eq!(v["task_state"], "open");
    assert_eq!(v["channel_state"], "open");
    assert_eq!(v["can_reply"], true);
    assert_eq!(v["coordinator_wake_state"], "scheduled");
    assert_eq!(v["files"][0]["resource"], f.file);
    assert!(s.read_task(&f.c.user, &f.ta).is_err());
    assert!(s.read_inquiry(&f.d.user, &f.tb, i64::MAX).is_err());
    assert!(s.read_note(&f.c.user, &f.file).is_err());
    // More than one page, newest page first but messages chronological inside each page.
    for n in 0..35 {
        let w = work(s, &f.c.recipient, &format!("Repair update {n}"));
        s.attach_task_reply(&w, &f.ta).unwrap();
        s.finish(&w, "[WAIT]").unwrap();
    }
    let v = s.read_inquiry(&f.c.user, &f.ta, i64::MAX).unwrap();
    assert_eq!(v["history_truncated"], true);
    assert_eq!(v["messages"].as_array().unwrap().len(), 30);
    let older = s
        .read_inquiry(&f.c.user, &f.ta, v["before_cursor"].as_i64().unwrap())
        .unwrap();
    assert!(older.to_string().contains("Alex here"));
    let close = work(s, &f.a.recipient, "Complete the repair task");
    let rev = s.read_task(&f.a.user, &f.ta).unwrap()["revision"]
        .as_i64()
        .unwrap();
    s.update_task(&close, &f.ta, rev, "Completed", "completed")
        .unwrap();
    s.finish(&close, "[WAIT]").unwrap();
    let v = s.read_inquiry(&f.c.user, &f.ta, i64::MAX).unwrap();
    assert_eq!(v["task_state"], "completed");
    assert_eq!(v["channel_state"], "open");
    assert_eq!(v["can_reply"], false);
    assert_eq!(v["coordinator_wake_state"], "none_pending");
}
#[tokio::test]
async fn quoted_text_and_file_replies_route_without_model_and_reject_spoofed_stale_or_forwarded_context(
) {
    let f = fixture();
    let s = &f.rt.store;
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE agent_task_messages SET state='sending' WHERE stanza=?",
            [&f.qa],
        )
        .unwrap();
    let w = quote(s, &f.c.recipient, "Where is the door repair?", &f.qa, true);
    f.rt.process_message(&w, None).await.unwrap();
    assert!(received(s, &f.ta, w.id));
    assert!(!received(s, &f.tb, w.id));
    assert!(!s
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM outbox WHERE inbox_id=?)",
            [w.id],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    let w = quote(
        s,
        &f.c.recipient,
        "Question about this file",
        &f.file_quote,
        true,
    );
    f.rt.process_message(&w, None).await.unwrap();
    assert!(received(s, &f.ta, w.id));
    for (identity, allowed) in [(&f.d.recipient, true), (&f.c.recipient, false)] {
        let w = quote(s, identity, "Must not be forwarded", &f.qa, allowed);
        f.rt.process_message(&w, None).await.unwrap();
        assert!(!received(s, &f.ta, w.id));
    }
    let w = work(s, &f.b.recipient, "Revoke contact");
    s.revoke_task_contact(&w, &f.tb, &f.c.user).unwrap();
    s.finish(&w, "[WAIT]").unwrap();
    let w = quote(s, &f.c.recipient, "Closed inquiry reply", &f.qb, true);
    f.rt.process_message(&w, None).await.unwrap();
    assert!(!received(s, &f.tb, w.id));
}
#[test]
fn clarification_persists_original_message_and_resolves_once_after_restart() {
    let mut f = fixture();
    let s = &f.rt.store;
    let original = work(s, &f.c.recipient, "Tomorrow works");
    s.ask_inquiry_clarification(
        &original,
        &[f.ta.clone(), f.tb.clone()],
        "Door repair or camera installation?",
    )
    .unwrap();
    s.finish(&original, "Model text cannot replace the factual question")
        .unwrap();
    assert!(!received(s, &f.ta, original.id) && !received(s, &f.tb, original.id));
    let question: String =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT text FROM outbox WHERE inbox_id=?",
                [original.id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(question, "Door repair or camera installation?");
    drop(f.rt);
    f.rt = Arc::try_unwrap(
        Runtime::open_with_llm(
            f.dir.path(),
            50,
            500,
            Llm::new("http://127.0.0.1:9/v1".into(), "test".into()).unwrap(),
        )
        .unwrap(),
    )
    .ok()
    .unwrap();
    let s = &f.rt.store;
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE settings SET value=CAST(value AS INTEGER)+1 WHERE key='authority_epoch'",
            [],
        )
        .unwrap();
    let selection = work(s, &f.c.recipient, "Camera installation");
    assert!(s
        .pending_inquiry_clarification(&selection)
        .unwrap()
        .unwrap()
        .to_string()
        .contains("Tomorrow works"));
    assert!(s
        .resolve_inquiry_clarification(&selection, &f.ta.replace('-', "x"))
        .is_err());
    s.resolve_inquiry_clarification(&selection, &f.tb).unwrap();
    assert!(!received(s, &f.tb, original.id));
    s.finish(&selection, "[WAIT]").unwrap();
    assert!(received(s, &f.tb, original.id));
    assert!(!received(s, &f.ta, original.id));
    assert!(!received(s, &f.tb, selection.id));
    let replay = work(s, &f.c.recipient, "Camera installation");
    assert!(s.resolve_inquiry_clarification(&replay, &f.tb).is_err());
    s.finish(&replay, "Private message").unwrap();
}
#[test]
fn failed_and_interrupted_decisions_never_publish_and_revoked_candidates_stay_private() {
    let mut f = fixture();
    let s = &f.rt.store;
    let w = work(s, &f.c.recipient, "Door repair: what address?");
    s.attach_task_reply(&w, &f.ta).unwrap();
    assert!(!s
        .read_task(&f.a.user, &f.ta)
        .unwrap()
        .to_string()
        .contains(&w.text));
    s.cancel_task_actions(&w).unwrap();
    s.finish(&w, "Failed").unwrap();
    assert!(!received(s, &f.ta, w.id));
    let original = work(s, &f.c.recipient, "Tomorrow works");
    s.ask_inquiry_clarification(&original, &[f.ta.clone(), f.tb.clone()], "Which inquiry?")
        .unwrap();
    s.finish(&original, "Question").unwrap();
    let selection = work(s, &f.c.recipient, "Installation");
    s.resolve_inquiry_clarification(&selection, &f.tb).unwrap();
    drop(f.rt);
    f.rt = Arc::try_unwrap(
        Runtime::open_with_llm(
            f.dir.path(),
            50,
            500,
            Llm::new("http://127.0.0.1:9/v1".into(), "test".into()).unwrap(),
        )
        .unwrap(),
    )
    .ok()
    .unwrap();
    let s = &f.rt.store;
    assert!(!received(s, &f.tb, original.id));
    let w = work(s, &f.b.recipient, "Revoke contact");
    s.revoke_task_contact(&w, &f.tb, &f.c.user).unwrap();
    s.finish(&w, "[WAIT]").unwrap();
    let w = work(s, &f.c.recipient, "Installation");
    assert!(s.resolve_inquiry_clarification(&w, &f.tb).is_err());
    s.cancel_inquiry_clarification(&w).unwrap();
    s.finish(&w, "Cancelled").unwrap();
    assert!(!received(s, &f.tb, original.id));
}
#[test]
fn owner_budget_exhaustion_defers_wake_without_losing_reply_and_rechecks_revocation() {
    for revoke in [false, true] {
        let f = fixture();
        let s = &f.rt.store;
        s.db.lock()
            .unwrap()
            .execute("UPDATE quotas SET used=50 WHERE user_id=?", [&f.a.user])
            .unwrap();
        let reply = work(s, &f.c.recipient, "Repair question");
        s.attach_task_reply(&reply, &f.ta).unwrap();
        s.finish(&reply, "[WAIT]").unwrap();
        assert!(received(s, &f.ta, reply.id));
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM inquiry_pending_wakes", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        s.db.lock()
            .unwrap()
            .execute("UPDATE quotas SET used=0 WHERE user_id=?", [&f.a.user])
            .unwrap();
        if revoke {
            let w = work(s, &f.a.recipient, "Revoke contact");
            s.revoke_task_contact(&w, &f.ta, &f.c.user).unwrap();
            s.finish(&w, "[WAIT]").unwrap();
        }
        s.tick_tasks(0).unwrap();
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM inquiry_pending_wakes", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let woke=s.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM inbox i JOIN agent_task_events e ON e.inbox=i.id WHERE e.task=? AND i.state='pending')",[&f.ta],|r|r.get::<_,bool>(0)).unwrap();
        assert_eq!(woke, !revoke);
    }
}
struct Provider {
    step: AtomicUsize,
    a: String,
    b: String,
}
async fn mock_model(State(p): State<Arc<Provider>>, Json(body): Json<Value>) -> Json<Value> {
    let n = p.step.fetch_add(1, Ordering::SeqCst);
    let raw = body.to_string();
    for secret in [
        "OWNER_OBJECTIVE_CANARY",
        "OWNER_PRIVATE_NOTE_CANARY",
        "OTHER_RECIPIENT_TERMS_CANARY",
        "PRIVATE_TIMER_PURPOSE_CANARY",
        "OTHER_OWNER_PRIVATE_NOTE_CANARY",
    ] {
        assert!(!raw.contains(secret));
    }
    let calls = match n {
        0 => vec![
            ("read_inquiry", json!({"task":p.a,"before":i64::MAX})),
            ("read_inquiry", json!({"task":p.b,"before":i64::MAX})),
        ],
        1 => vec![(
            "ask_inquiry_clarification",
            json!({"tasks":[p.a,p.b],"question":"Door repair or camera installation?"}),
        )],
        2 => vec![],
        3 => {
            assert!(raw.contains("Tomorrow works"));
            vec![("resolve_inquiry_clarification", json!({"task":p.b}))]
        }
        _ => vec![],
    };
    Json(if calls.is_empty() {
        json!({"choices":[{"message":{"role":"assistant","content":if n==2{"Wrong model question must be replaced"}else{"[WAIT]"}}}]})
    } else {
        json!({"choices":[{"message":{"role":"assistant","tool_calls":calls.into_iter().enumerate().map(|(i,(name,args))|json!({"id":format!("{n}-{i}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect::<Vec<_>>()}}]})
    })
}
#[tokio::test]
async fn actual_runtime_model_loop_ambiguity_then_selection_preserves_privacy_e2e() {
    let mut f = fixture();
    let p = Arc::new(Provider {
        step: AtomicUsize::new(0),
        a: f.ta.clone(),
        b: f.tb.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm = Llm::new(
        format!("http://{}/v1", listener.local_addr().unwrap()),
        "test".into(),
    )
    .unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_model))
        .with_state(p.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    f.rt.llm = llm;
    let original = work(&f.rt.store, &f.c.recipient, "Tomorrow works");
    f.rt.process_message(&original, Some("fixture".into()))
        .await
        .unwrap();
    assert_eq!(p.step.load(Ordering::SeqCst), 3);
    assert!(
        !received(&f.rt.store, &f.ta, original.id) && !received(&f.rt.store, &f.tb, original.id)
    );
    let selection = work(&f.rt.store, &f.c.recipient, "Camera installation");
    f.rt.process_message(&selection, Some("fixture".into()))
        .await
        .unwrap();
    assert_eq!(p.step.load(Ordering::SeqCst), 5);
    assert!(received(&f.rt.store, &f.tb, original.id));
    assert!(!received(&f.rt.store, &f.ta, original.id));
    server.abort();
}
#[tokio::test]
#[ignore = "paid local-proxy multi-inquiry acceptance; requires CARBON_E2E_LIVE=1"]
async fn live_model_multi_inquiry_continuity_acceptance() {
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
        std::env::var("CARBON_E2E_ARTIFACT_DIR").expect("Fresh artifact directory required"),
    );
    assert!(!artifacts.join("report.json").exists());
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut f = fixture();
    f.rt.llm = Llm::new(upstream.clone(), "test".into()).unwrap();
    let mut operations = Vec::new();
    let mut checks = Vec::new();
    fn capture(
        rt: &Runtime,
        w: &Work,
        artifacts: &std::path::Path,
        index: usize,
        operations: &mut Vec<Value>,
    ) -> Value {
        let db = rt.store.db.lock().unwrap();
        let operation: String = db
            .query_row(
                "SELECT operation_id FROM inbox_operations WHERE inbox_id=?",
                [w.id],
                |r| r.get(0),
            )
            .unwrap();
        let reply: Option<String> = db
            .query_row("SELECT text FROM outbox WHERE inbox_id=?", [w.id], |r| {
                r.get(0)
            })
            .optional()
            .unwrap();
        drop(db);
        let trace = rt.store.trace_export(&operation).unwrap();
        std::fs::write(
            artifacts.join(format!("operation-{index:02}.json")),
            serde_json::to_vec_pretty(&trace).unwrap(),
        )
        .unwrap();
        operations.push(json!({"operation":operation,"message":w.text,"reply":reply,"trace_file":format!("operation-{index:02}.json")}));
        trace
    }
    let original = work(&f.rt.store, &f.c.recipient, "Tomorrow works.");
    f.rt.process_message(&original, Some(model.clone()))
        .await
        .unwrap();
    capture(&f.rt, &original, &artifacts, 0, &mut operations);
    let pending =
        f.rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM inquiry_clarifications WHERE inbox=? AND state='pending'",
                [original.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
    checks.push((
        "ambiguous_message_held_and_clarification_persisted",
        pending == 1
            && !received(&f.rt.store, &f.ta, original.id)
            && !received(&f.rt.store, &f.tb, original.id),
    ));
    drop(f.rt);
    f.rt = Arc::try_unwrap(
        Runtime::open_with_llm(
            f.dir.path(),
            50,
            500,
            Llm::new(upstream, "test".into()).unwrap(),
        )
        .unwrap(),
    )
    .ok()
    .unwrap();
    let private = work(
        &f.rt.store,
        &f.c.recipient,
        "Tell me a short joke about bananas.",
    );
    f.rt.process_message(&private, Some(model.clone()))
        .await
        .unwrap();
    capture(&f.rt, &private, &artifacts, 1, &mut operations);
    checks.push((
        "unrelated_request_stays_private",
        !received(&f.rt.store, &f.ta, private.id)
            && !received(&f.rt.store, &f.tb, private.id)
            && f.rt
                .store
                .pending_inquiry_clarification(&private)
                .unwrap()
                .is_some(),
    ));
    let selection = work(&f.rt.store, &f.c.recipient, "Camera installation.");
    f.rt.process_message(&selection, Some(model.clone()))
        .await
        .unwrap();
    capture(&f.rt, &selection, &artifacts, 2, &mut operations);
    checks.push((
        "selection_after_restart_routes_original_once_to_camera",
        received(&f.rt.store, &f.tb, original.id)
            && !received(&f.rt.store, &f.ta, original.id)
            && !received(&f.rt.store, &f.tb, selection.id),
    ));
    let question = work(
        &f.rt.store,
        &f.c.recipient,
        "For Alex's door repair, what's the address?",
    );
    f.rt.process_message(&question, Some(model.clone()))
        .await
        .unwrap();
    capture(&f.rt, &question, &artifacts, 3, &mut operations);
    checks.push((
        "ordinary_question_continues_repair_without_reverse_contact",
        received(&f.rt.store, &f.ta, question.id) && !received(&f.rt.store, &f.tb, question.id),
    ));
    let owned =
        f.rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_tasks WHERE owner=? AND source=?",
                params![f.c.user, question.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
    checks.push(("no_new_address_helper_task", owned == 0));
    let status = work(
        &f.rt.store,
        &f.c.recipient,
        "What is the status of Alex's door repair inquiry?",
    );
    f.rt.process_message(&status, Some(model.clone()))
        .await
        .unwrap();
    let status_trace = capture(&f.rt, &status, &artifacts, 4, &mut operations);
    let status_reply = operations.last().unwrap()["reply"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    let grounded = status_trace["entries"].as_array().unwrap().iter().any(|e| {
        e["kind"] == "tool_result"
            && e["data"]["call"]["function"]["name"] == "read_inquiry"
            && e["data"]["ok"] == true
            && e["data"]["result"]["task"] == f.ta
            && e["data"]["result"]["task_state"] == "open"
    }) || received(&f.rt.store, &f.ta, status.id);
    checks.push((
        "status_grounded_in_inquiry_not_completed_helper",
        grounded
            && ![
                "follow-up is closed",
                "inquiry is closed",
                "repair is completed",
                "repair is complete",
            ]
            .iter()
            .any(|s| status_reply.contains(s)),
    ));
    let quoted = quote(
        &f.rt.store,
        &f.c.recipient,
        "Any update on this?",
        &f.qb,
        true,
    );
    f.rt.process_message(&quoted, Some(model.clone()))
        .await
        .unwrap();
    let trace = capture(&f.rt, &quoted, &artifacts, 5, &mut operations);
    checks.push((
        "quoted_reply_deterministic_without_model",
        received(&f.rt.store, &f.tb, quoted.id)
            && trace["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["kind"] != "model_request"),
    ));
    let close = work(&f.rt.store, &f.b.recipient, "Complete camera task");
    let rev = f.rt.store.read_task(&f.b.user, &f.tb).unwrap()["revision"]
        .as_i64()
        .unwrap();
    f.rt.store
        .update_task(&close, &f.tb, rev, "Completed", "completed")
        .unwrap();
    f.rt.store.finish(&close, "[WAIT]").unwrap();
    let stale = quote(
        &f.rt.store,
        &f.c.recipient,
        "Closed inquiry question",
        &f.qb,
        true,
    );
    f.rt.process_message(&stale, Some(model.clone()))
        .await
        .unwrap();
    capture(&f.rt, &stale, &artifacts, 6, &mut operations);
    checks.push((
        "closed_quoted_inquiry_not_rerouted",
        !received(&f.rt.store, &f.tb, stale.id) && !received(&f.rt.store, &f.ta, stale.id),
    ));
    let mut no_private = true;
    for i in 0..operations.len() {
        let trace: Value = serde_json::from_slice(
            &std::fs::read(artifacts.join(format!("operation-{i:02}.json"))).unwrap(),
        )
        .unwrap();
        for e in trace["entries"].as_array().unwrap() {
            if e["kind"] == "model_request" {
                let body = e["data"]["body"].to_string();
                no_private &= ![
                    "OWNER_OBJECTIVE_CANARY",
                    "OWNER_PRIVATE_NOTE_CANARY",
                    "OTHER_RECIPIENT_TERMS_CANARY",
                    "PRIVATE_TIMER_PURPOSE_CANARY",
                    "OTHER_OWNER_PRIVATE_NOTE_CANARY",
                ]
                .iter()
                .any(|s| body.contains(s));
            }
        }
    }
    checks.push((
        "no_owner_private_other_recipient_or_timer_canaries_in_model_context",
        no_private,
    ));
    let passed = checks.iter().all(|(_, ok)| *ok);
    std::fs::write(artifacts.join("report.json"),serde_json::to_vec_pretty(&json!({"model":model,"passed":passed,"checks":checks,"operations":operations,"limitations":["Text and already-delivered outbound-file metadata","WhatsApp transport simulated","Incoming attachment content forwarding is not tested","Association of unquoted messages remains model judgment"]})).unwrap()).unwrap();
    assert!(
        passed,
        "Inspect live inquiry report and full operation traces"
    );
}
#[test]
fn revocation_between_selection_and_finish_cancels_reply_without_sticking_worker() {
    let f = fixture();
    let s = &f.rt.store;
    let reply = work(s, &f.c.recipient, "Door repair: new question");
    s.attach_task_reply(&reply, &f.ta).unwrap();
    let revoke = work(s, &f.a.recipient, "Revoke contact");
    s.revoke_task_contact(&revoke, &f.ta, &f.c.user).unwrap();
    s.finish(&revoke, "[WAIT]").unwrap();
    f.rt.finish_processed_turn(&reply, "[WAIT]");
    assert!(!received(s, &f.ta, reply.id));
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row("SELECT state FROM inbox WHERE id=?", [reply.id], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "done"
    );
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_task_routes WHERE inbox=?",
                [reply.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn clarification_cue_spoofing_replay_expiry_and_unrelated_private_message() {
    let f = fixture();
    let s = &f.rt.store;
    let original = work(s, &f.c.recipient, "Tomorrow works");
    s.ask_inquiry_clarification(&original, &[f.ta.clone(), f.tb.clone()], "Which inquiry?")
        .unwrap();
    s.finish(&original, "Question").unwrap();
    let (oid,cue):(i64,String)=s.db.lock().unwrap().query_row("SELECT o.id,c.stanza FROM inquiry_clarifications c JOIN outbox o ON o.inbox_id=c.inbox WHERE c.inbox=?",[original.id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    s.sent(oid, true).unwrap();
    let spoof = quote(s, &f.d.recipient, "Installation", &cue, true);
    f.rt.process_message(&spoof, None).await.unwrap();
    assert!(!received(s, &f.tb, original.id));
    let private = work(s, &f.c.recipient, "A private request");
    assert!(s.pending_inquiry_clarification(&private).unwrap().is_some());
    s.finish(&private, "Private response").unwrap();
    assert!(!received(s, &f.ta, private.id) && !received(s, &f.tb, private.id));
    let selection = quote(s, &f.c.recipient, "Installation", &cue, true);
    assert!(s.attach_task_reply(&selection, &f.tb).is_err());
    s.resolve_inquiry_clarification(&selection, &f.tb).unwrap();
    s.finish(&selection, "[WAIT]").unwrap();
    assert!(received(s, &f.tb, original.id));
    let replay = quote(s, &f.c.recipient, "Door repair instead", &cue, true);
    f.rt.process_message(&replay, None).await.unwrap();
    assert!(!received(s, &f.ta, replay.id));
    let expired = work(s, &f.c.recipient, "Other ambiguous message");
    s.ask_inquiry_clarification(&expired, &[f.ta.clone(), f.tb.clone()], "Which inquiry?")
        .unwrap();
    s.finish(&expired, "Question").unwrap();
    let cue: String =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT stanza FROM inquiry_clarifications WHERE inbox=?",
                [expired.id],
                |r| r.get(0),
            )
            .unwrap();
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE inquiry_clarifications SET expires=0 WHERE inbox=?",
            [expired.id],
        )
        .unwrap();
    let late = quote(s, &f.c.recipient, "Door repair", &cue, true);
    f.rt.process_message(&late, None).await.unwrap();
    assert!(!received(s, &f.ta, expired.id));
}
#[test]
fn held_ambiguity_turn_cannot_publish_parallel_outreach_or_files() {
    let f = fixture();
    let s = &f.rt.store;
    let w = work(s, &f.c.recipient, "Tomorrow works");
    let t = s.create_task(&w, "An unrelated proposed outreach").unwrap()["task"]
        .as_str()
        .unwrap()
        .to_string();
    s.authorize_task_contact(&w, &t, &f.a.user, &w.text)
        .unwrap();
    s.open_task_channel(&w, &t, "", &f.a.user).unwrap();
    s.send_task_message(&w, &t, &f.a.user, "Must remain unsent")
        .unwrap();
    let file = s
        .save_note(&f.c.user, "Held file", "Must remain unsent")
        .unwrap();
    s.send_task_file(&w, &t, &f.a.user, &file).unwrap();
    s.ask_inquiry_clarification(&w, &[f.ta.clone(), f.tb.clone()], "Which inquiry?")
        .unwrap();
    s.finish(&w, "Question").unwrap();
    assert!(s.claim_task_send().unwrap().is_none());
    assert!(s.claim_file().unwrap().is_none());
    assert!(!received(s, &f.ta, w.id) && !received(s, &f.tb, w.id));
}
#[tokio::test]
async fn ordinary_unbound_approval_words_are_not_misclassified_as_admin_controls() {
    let mut f = fixture();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app=Router::new().route("/v1/chat/completions",post(move || {let count=seen.clone();async move{count.fetch_add(1,Ordering::SeqCst);Json(json!({"choices":[{"message":{"role":"assistant","content":"Ordinary conversation response."}}]}))}}));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    f.rt.llm = Llm::new(base, "test".into()).unwrap();
    for (index, text) in ["approve", "approve the plan"].iter().enumerate() {
        let w = work(&f.rt.store, &f.c.recipient, text);
        assert!(!f.rt.store.admin_control_bound(&w).unwrap());
        f.rt.process_message(&w, Some("fixture".into()))
            .await
            .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), index + 1);
        assert_eq!(
            f.rt.store
                .db
                .lock()
                .unwrap()
                .query_row("SELECT text FROM outbox WHERE inbox_id=?", [w.id], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "Ordinary conversation response."
        );
    }
    server.abort();
}
