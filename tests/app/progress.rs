use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize},
    Mutex,
};
#[derive(Default)]
struct Fake {
    block_pause: AtomicBool,
    events: Mutex<Vec<&'static str>>,
    invalid: AtomicBool,
    disconnected: AtomicBool,
    failure: AtomicBool,
    notices: AtomicUsize,
}
impl Feedback for Arc<Fake> {
    fn current(&self) -> bool {
        !self.disconnected.load(Ordering::SeqCst)
    }
    fn allowed(&self) -> bool {
        self.current() && !self.invalid.load(Ordering::SeqCst)
    }
    async fn presence(&self, typing: bool) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(if typing { "typing" } else { "paused" });
        if !typing && self.block_pause.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.failure.load(Ordering::SeqCst) {
            anyhow::bail!("transport error")
        };
        Ok(())
    }
    async fn notice(&self) -> Result<()> {
        self.notices.fetch_add(1, Ordering::SeqCst);
        if self.failure.load(Ordering::SeqCst) {
            anyhow::bail!("notice failure")
        };
        Ok(())
    }
}
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn fast_reply_and_failure_clear_typing_without_notice() {
    for fail in [false, true] {
        let fake = Arc::new(Fake::default());
        let (tx, rx) = oneshot::channel();
        let copy = fake.clone();
        let task = tokio::spawn(during(copy, async move {
            rx.await?;
            if fail {
                anyhow::bail!("model failed")
            };
            Ok("reply")
        }));
        settle().await;
        assert_eq!(*fake.events.lock().unwrap(), vec!["typing"]);
        tx.send(()).unwrap();
        assert_eq!(task.await.unwrap().is_err(), fail);
        assert_eq!(*fake.events.lock().unwrap(), vec!["typing", "paused"]);
        assert_eq!(fake.notices.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test(start_paused = true)]
async fn retry_wait_refreshes_typing_once_notice_and_feedback_failure_is_nonfatal() {
    let fake = Arc::new(Fake::default());
    fake.failure.store(true, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(during(fake.clone(), async move {
        rx.await?;
        Ok("eventual reply")
    }));
    settle().await;
    for _ in 0..12 {
        tokio::time::advance(Duration::from_secs(10)).await;
        settle().await;
    }
    assert!(
        fake.events
            .lock()
            .unwrap()
            .iter()
            .filter(|x| **x == "typing")
            .count()
            > 5
    );
    assert_eq!(fake.notices.load(Ordering::SeqCst), 1);
    tx.send(()).unwrap();
    assert_eq!(task.await.unwrap().unwrap(), "eventual reply");
    assert_eq!(fake.events.lock().unwrap().last(), Some(&"paused"));
}
#[tokio::test(start_paused = true)]
async fn dropped_work_revocation_and_disconnect_stop_background_feedback() {
    for mode in ["drop", "revoke", "disconnect"] {
        let fake = Arc::new(Fake::default());
        let task = tokio::spawn(during(fake.clone(), std::future::pending::<Result<()>>()));
        settle().await;
        match mode {
            "drop" => task.abort(),
            "revoke" => fake.invalid.store(true, Ordering::SeqCst),
            _ => fake.disconnected.store(true, Ordering::SeqCst),
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        if mode == "drop" {
            assert!(task.await.is_err());
        } else {
            assert!(task.await.unwrap().is_err());
        }
        let count = fake.events.lock().unwrap().len();
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(fake.events.lock().unwrap().len(), count);
        assert_eq!(fake.notices.load(Ordering::SeqCst), 0);
        assert_eq!(
            fake.events.lock().unwrap().last(),
            Some(&if mode == "disconnect" {
                "typing"
            } else {
                "paused"
            })
        );
    }
}
#[tokio::test(start_paused = true)]
async fn overall_deadline_ends_a_stuck_operation() {
    let fake = Arc::new(Fake::default());
    let task = tokio::spawn(during(fake.clone(), std::future::pending::<Result<()>>()));
    settle().await;
    tokio::time::advance(TURN_TIMEOUT).await;
    settle().await;
    assert!(task.await.unwrap().is_err());
    assert_eq!(fake.events.lock().unwrap().last(), Some(&"paused"));
}
#[test]
fn notice_reservation_is_actor_bound_durable_and_not_conversation_history() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    s.ingest("alice@test", "request", "hello").unwrap();
    let w = s.next().unwrap().unwrap();
    let mut forged = w.clone();
    forged.recipient = "bob@test".into();
    assert!(s.reserve_progress(&Target::Private(forged)).is_err());
    let target = Target::Private(w.clone());
    assert!(s.reserve_progress(&target).unwrap().is_some());
    assert!(s.reserve_progress(&target).unwrap().is_none());
    assert!(!s
        .history(&w.user)
        .unwrap()
        .iter()
        .any(|x| x.to_string().contains(NOTICE)));
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE settings SET value='1' WHERE key='authority_epoch'",
            [],
        )
        .unwrap();
    assert!(s.reserve_progress(&target).is_err());
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    assert!(s.reserve_progress(&target).is_err());
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM progress_notices WHERE state='unknown'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(s.claim_send().unwrap().is_none());
}
#[tokio::test]
async fn proxy_response_after_old_90_second_limit_can_finish() {
    use axum::{routing::post, Json, Router};
    use serde_json::json;
    let received = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let signal = received.clone();
    let gate = release.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let signal = signal.clone();
            let gate = gate.clone();
            async move {
                signal.notify_one();
                gate.notified().await;
                Json(json!({"choices":[{"message":{"content":"Finished after proxy retry"}}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let llm = crate::llm::Llm::new(format!("http://{address}/v1"), "test".into()).unwrap();
    let call = tokio::spawn(async move {
        let response = llm
            .client
            .post(format!("{}/chat/completions", llm.base))
            .json(&json!({"model":"test"}))
            .send()
            .await?;
        llm.decode(response).await
    });
    received.notified().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(100)).await;
    settle().await;
    assert!(!call.is_finished());
    tokio::time::resume();
    release.notify_one();
    assert!(call
        .await
        .unwrap()
        .unwrap()
        .to_string()
        .contains("Finished after proxy retry"));
    server.abort();
    assert_eq!(crate::llm::REQUEST_TIMEOUT, Duration::from_secs(300));
}

#[tokio::test(start_paused = true)]
async fn cancellation_drops_model_before_slow_paused_delivery() {
    let fake = Arc::new(Fake::default());
    fake.block_pause.store(true, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(during(fake.clone(), async move {
        rx.await?;
        Ok(())
    }));
    settle().await;
    fake.invalid.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(
        tx.send(()).is_err(),
        "Model future survived cancellation while presence cleanup waited"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(task.await.unwrap().is_err());
}
