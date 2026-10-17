//! Actual intake, atomic production dispatcher, model loop, permissions and outbox.
//! Only WhatsApp delivery and provider latency/content are simulated.
use crate::{
    concurrency::{Job, ProcessingPool},
    ingress::tests::parsed,
    llm::Llm,
    runtime::Runtime,
};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use whatsapp_rust::prelude::{wa, MessageBuilderExt};

async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Concurrent operation did not reach expected state");
}
async fn ingest(rt: &Runtime, account: &str, id: &str, text: &str) {
    rt.receive_message(&parsed(account, id), &wa::Message::text(text))
        .await;
}
fn runner(
    rt: Arc<Runtime>,
) -> impl Fn(Job) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    move |job| {
        let rt = rt.clone();
        Box::pin(async move {
            let Job::Private(work) = job else {
                panic!("Unexpected legacy pair")
            };
            rt.process_message(&work, Some("mock".into()))
                .await
                .unwrap();
        })
    }
}
struct Provider {
    gate: Semaphore,
    blocked: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    notes: Mutex<(String, String)>,
}
async fn model(State(p): State<Arc<Provider>>, Json(body): Json<Value>) -> Json<Value> {
    p.requests.lock().unwrap().push(body.clone());
    let messages = body["messages"].as_array().unwrap();
    let input = messages.iter().rfind(|m| m["role"] == "user").unwrap()["content"]
        .as_str()
        .unwrap();
    let tool = messages.last().unwrap()["role"] == "tool";
    let (a, b) = p.notes.lock().unwrap().clone();
    let name = if input.contains("A_FIRST") {
        "A_FIRST"
    } else if input.contains("A_SECOND") {
        "A_SECOND"
    } else {
        "B_FIRST"
    };
    let call = if !tool {
        if name == "A_FIRST" {
            p.blocked.fetch_add(1, Ordering::SeqCst);
            p.gate.acquire().await.unwrap().forget();
        }
        Some(if name == "B_FIRST" { a.clone() } else { a })
    } else if name == "B_FIRST"
        && messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("error")
    {
        Some(b)
    } else {
        None
    };
    Json(match call {
        Some(id) => {
            json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":format!("read-{}",messages.len()),"type":"function","function":{"name":"read_note","arguments":json!({"id":id}).to_string()}}]}}]})
        }
        None => {
            json!({"choices":[{"message":{"role":"assistant","content":format!("{name}_DONE")}}]})
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_users_slow_provider_ordered_replies_scheduler_and_private_acl_e2e() {
    let p = Arc::new(Provider {
        gate: Semaphore::new(0),
        blocked: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        notes: Mutex::new((String::new(), String::new())),
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
    let dir = tempfile::tempdir().unwrap();
    let rt = Runtime::open_with_llm(dir.path(), 50, 500, llm).unwrap();
    rt.store.db.lock().unwrap().execute_batch("INSERT INTO settings VALUES('experimental_tasks','1'); INSERT INTO settings VALUES('debug_trace','1');").unwrap();
    ingest(&rt, "15550000001@s.whatsapp.net", "a1", "A_FIRST").await;
    ingest(&rt, "15550000001@s.whatsapp.net", "a2", "A_SECOND").await;
    ingest(&rt, "15550000002@s.whatsapp.net", "b1", "B_FIRST").await;
    let a: String = rt
        .store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT user_id FROM identities WHERE identity='15550000001@s.whatsapp.net'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let b: String = rt
        .store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT user_id FROM identities WHERE identity='15550000002@s.whatsapp.net'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    *p.notes.lock().unwrap() = (
        rt.store
            .save_note(&a, "private a", "A_SECRET_CANARY")
            .unwrap(),
        rt.store
            .save_note(&b, "private b", "B_SECRET_CANARY")
            .unwrap(),
    );
    let mut pool = ProcessingPool::new(rt.store.clone(), 2);
    assert_eq!(pool.fill(true, runner(rt.clone())).unwrap(), 2);
    wait_for(|| p.blocked.load(Ordering::SeqCst) == 1).await;
    // B's real model loop must finish and deliver while A's HTTP response is withheld.
    wait_for(|| {
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM outbox WHERE text='B_FIRST_DONE')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
    })
    .await;
    let (id, recipient, text) = rt.store.claim_send().unwrap().unwrap();
    assert_eq!(recipient, "15550000002@s.whatsapp.net");
    assert_eq!(text, "B_FIRST_DONE");
    rt.store.sent(id, true).unwrap();
    assert!(!p
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|v| v.to_string().contains("A_SECOND")));
    let requests = p.requests.lock().unwrap().clone();
    let b_requests: Vec<_> = requests
        .iter()
        .filter(|v| v.to_string().contains("B_FIRST"))
        .collect();
    assert!(
        b_requests.iter().any(|v| v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("error"))),
        "Foreign private read must be rejected"
    );
    assert!(b_requests
        .iter()
        .any(|v| v.to_string().contains("B_SECRET_CANARY")));
    assert!(b_requests
        .iter()
        .all(|v| !v.to_string().contains("A_SECRET_CANARY")));
    // Exercise the real independent scheduler while A is still blocked.
    ingest(
        &rt,
        "15550000003@s.whatsapp.net",
        "timer",
        "Schedule a check",
    )
    .await;
    let w = rt.store.claim_private(false).unwrap().unwrap();
    let task = rt.store.create_task(&w, "Check later").unwrap();
    rt.store
        .schedule_task(&w, task["task"].as_str().unwrap(), 60, "Check later")
        .unwrap();
    rt.store.finish(&w, "[WAIT]").unwrap();
    rt.store
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_task_wakes SET due=unixepoch()-1 WHERE state='pending'",
            [],
        )
        .unwrap();
    let scheduler = tokio::spawn(rt.clone().scheduler_loop());
    wait_for(|| {
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_task_wakes WHERE state='fired')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
    })
    .await;
    scheduler.abort();
    assert_eq!(p.gate.available_permits(), 0);
    p.gate.add_permits(1);
    wait_for(|| {
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM outbox WHERE text='A_FIRST_DONE')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
    })
    .await;
    // Remove the scheduler fixture event, unrelated to the ordering acceptance.
    rt.store.db.lock().unwrap().execute("UPDATE inbox SET state='cancelled' WHERE state='pending' AND id IN(SELECT inbox FROM agent_task_events)",[]).unwrap();
    wait_for(|| {
        pool.reap();
        pool.len() == 0
    })
    .await;
    assert_eq!(pool.fill(true, runner(rt.clone())).unwrap(), 1);
    wait_for(|| {
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM outbox WHERE text='A_SECOND_DONE')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
    })
    .await;
    let first = rt.store.claim_send().unwrap().unwrap();
    rt.store.sent(first.0, true).unwrap();
    let second = rt.store.claim_send().unwrap().unwrap();
    rt.store.sent(second.0, true).unwrap();
    assert_eq!(first.2, "A_FIRST_DONE");
    assert_eq!(second.2, "A_SECOND_DONE");
    assert!(p
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v.to_string().contains("A_FIRST") || v.to_string().contains("A_SECOND"))
        .all(|v| !v.to_string().contains("B_SECRET_CANARY")));
    drop(pool);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_pool_cancellation_and_restart_never_publish_late_responses_e2e() {
    let p = Arc::new(Provider {
        gate: Semaphore::new(0),
        blocked: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        notes: Mutex::new((String::new(), String::new())),
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
    let dir = tempfile::tempdir().unwrap();
    let rt = Runtime::open_with_llm(dir.path(), 50, 500, llm).unwrap();
    rt.store
        .db
        .lock()
        .unwrap()
        .execute("INSERT INTO settings VALUES('experimental_tasks','1')", [])
        .unwrap();
    for n in 1..=5 {
        ingest(
            &rt,
            &format!("1555000000{n}@s.whatsapp.net"),
            "first",
            "A_FIRST",
        )
        .await;
    }
    ingest(&rt, "15550000001@s.whatsapp.net", "second", "A_SECOND").await;
    let mut pool = ProcessingPool::new(rt.store.clone(), 4);
    assert_eq!(pool.fill(true, runner(rt.clone())).unwrap(), 4);
    wait_for(|| p.blocked.load(Ordering::SeqCst) == 4).await;
    assert_eq!(pool.fill(true, runner(rt.clone())).unwrap(), 0);
    assert_eq!(
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM inbox WHERE state='processing'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    assert_eq!(
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM inbox WHERE state='pending'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    let stale:crate::store::Work=rt.store.db.lock().unwrap().query_row("SELECT id,user_id,recipient,text FROM inbox WHERE state='processing' ORDER BY id LIMIT 1",[],|r|Ok(crate::store::Work{id:r.get(0)?,user:r.get(1)?,recipient:r.get(2)?,text:r.get(3)?})).unwrap();
    drop(pool);
    assert!(rt.store.check_work(&stale).is_err());
    assert!(rt
        .store
        .finish(&stale, "A stale provider response")
        .is_err());
    wait_for(|| Arc::strong_count(&rt) == 1).await;
    p.gate.add_permits(4);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        rt.store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM inbox WHERE state='interrupted'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    drop(rt);
    let reopened = Runtime::open(dir.path(), 50, 500).unwrap();
    let fifth = reopened.store.claim_private(false).unwrap().unwrap();
    let second = reopened.store.claim_private(false).unwrap().unwrap();
    assert_eq!(fifth.recipient, "15550000005@s.whatsapp.net");
    assert_eq!(second.text, "A_SECOND");
    assert!(reopened.store.claim_private(false).unwrap().is_none());
    server.abort();
}

#[test]
fn private_and_legacy_pair_claims_cannot_overlap_participants() {
    use crate::store::Store;
    use rusqlite::params;
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    s.ingest("a@test", "a", "first").unwrap();
    let a = s.next().unwrap().unwrap();
    s.finish(&a, "ready").unwrap();
    s.ingest("b@test", "b", "hello").unwrap();
    let b = s.next().unwrap().unwrap();
    s.finish(&b, "ready").unwrap();
    {
        let db = s.db.lock().unwrap();
        db.execute(
            "INSERT INTO access_groups(id,name,owner) VALUES('a','A',?),('b','B',?)",
            params![a.user, a.user],
        )
        .unwrap();
        db.execute(
            "INSERT INTO memberships VALUES('a',?),('b',?)",
            params![a.user, b.user],
        )
        .unwrap();
        db.execute(
            "INSERT INTO coordination_scopes(id,requesters,recipients) VALUES('scope','a','b')",
            [],
        )
        .unwrap();
    }
    s.ingest("a@test", "new", "Contact B").unwrap();
    let source = s.claim_private(false).unwrap().unwrap();
    s.start_conversation(&source, "scope", &b.user).unwrap();
    s.finish(&source, "queued").unwrap();
    let pair = s.claim_pair().unwrap().unwrap();
    s.ingest("b@test", "new", "A private question").unwrap();
    assert!(s.claim_private(false).unwrap().is_none());
    s.conversation_result(pair.id, "failed").unwrap();
    let private = s.claim_private(false).unwrap().unwrap();
    s.ingest("a@test", "another", "Contact B again").unwrap();
    let next = s.next().unwrap().unwrap();
    s.start_conversation(&next, "scope", &b.user).unwrap();
    s.finish(&next, "queued").unwrap();
    assert!(s.claim_pair().unwrap().is_none());
    s.finish(&private, "done").unwrap();
    assert!(s.claim_pair().unwrap().is_some());
}

#[tokio::test]
async fn failed_worker_releases_participant_and_discards_staged_actions() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(crate::store::Store::open(dir.path()).unwrap());
    s.db.lock()
        .unwrap()
        .execute("INSERT INTO settings VALUES('experimental_tasks','1')", [])
        .unwrap();
    s.ingest("participant@test", "one", "Check later").unwrap();
    s.ingest("participant@test", "two", "Next request").unwrap();
    let mut pool = ProcessingPool::new(s.clone(), 1);
    let store = s.clone();
    assert_eq!(
        pool.fill(true, move |job| {
            let store = store.clone();
            async move {
                let Job::Private(w) = job else {
                    panic!("Unexpected pair")
                };
                let task = store.create_task(&w, "Follow up").unwrap();
                store
                    .schedule_task(&w, task["task"].as_str().unwrap(), 60, "Follow up")
                    .unwrap();
                panic!("Simulated processing panic after staging a schedule");
            }
        })
        .unwrap(),
        1
    );
    wait_for(|| {
        pool.reap();
        pool.len() == 0
    })
    .await;
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM agent_task_wakes WHERE state='cancelled'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let store = s.clone();
    assert_eq!(
        pool.fill(true, move |job| {
            let store = store.clone();
            async move {
                let Job::Private(w) = job else {
                    panic!("Unexpected pair")
                };
                assert_eq!(w.text, "Next request");
                store.finish(&w, "Completed next request").unwrap();
            }
        })
        .unwrap(),
        1
    );
    wait_for(|| {
        pool.reap();
        pool.len() == 0
    })
    .await;
    assert_eq!(s.claim_send().unwrap().unwrap().2, "Completed next request");
}
