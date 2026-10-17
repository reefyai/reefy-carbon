//! Application E2E: real parser -> live intake -> SQLite -> HTTP activity and durable reply.
//! WhatsApp's network/Signal layer and external model are deliberately outside this fixture.
use super::*;
use crate::ingress::tests::parsed;
use serde_json::Value;
use whatsapp_rust::prelude::*;

async fn activity(rt: Arc<runtime::Runtime>) -> Value {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app(Arc::new(AppState {
        credential_digest: Some(Sha256::digest(b"admin:synthetic-password").into()),
        failures: Mutex::new(HashMap::new()),
        runtime: Some(rt),
    }));
    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://{address}/api/activity");
    let unauthorized = client.get(&url).send().await.unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let response = client
        .get(&url)
        .basic_auth("admin", Some("synthetic-password"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let data = response.json().await.unwrap();
    drop(unauthorized);
    drop(client);
    shutdown.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    data
}

#[tokio::test]
async fn iphone_mac_ingress_e2e_activity_identity_dedup_reply_and_restart() {
    for account in ["100000000000001@lid", "15550000001@s.whatsapp.net"] {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime::Runtime::open(dir.path(), 50, 500).unwrap();
        let (user, domain) = account.split_once('@').unwrap();
        let mac = format!("{user}:7@{domain}");
        let phone_info = parsed(account, "PHONE");
        let mac_info = parsed(&mac, "MAC");
        let phone = wa::Message::text("PRIVATE_PHONE_CANARY");
        let desktop = wa::Message::text("PRIVATE_MAC_CANARY");
        rt.receive_message(&phone_info, &phone).await;
        rt.receive_message(&mac_info, &desktop).await;
        // A retransmit from another device must not create another job or user.
        rt.receive_message(&parsed(account, "MAC"), &desktop).await;
        let first = rt.store.next().unwrap().unwrap();
        let second = rt.store.next().unwrap().unwrap();
        assert_eq!(first.text, "PRIVATE_PHONE_CANARY");
        assert_eq!(second.text, "PRIVATE_MAC_CANARY");
        assert_eq!(first.user, second.user);
        assert_eq!(first.recipient, account);
        assert_eq!(second.recipient, account);
        assert!(rt.store.next().unwrap().is_none());
        assert_eq!(
            rt.store.participants().unwrap().as_array().unwrap().len(),
            1
        );
        // Exercise the real persisted response queue with a deterministic reply.
        for work in [&first, &second] {
            rt.store.finish(work, "Synthetic response").unwrap();
            let (id, recipient, response) = rt.store.claim_send().unwrap().unwrap();
            assert_eq!(recipient, account);
            assert_eq!(response, "Synthetic response");
            rt.store.sent(id, true).unwrap();
        }
        let data = activity(rt.clone()).await;
        let events = data["events"].as_array().unwrap();
        let received: Vec<_> = events
            .iter()
            .filter(|e| e["event"] == "message_received")
            .collect();
        assert_eq!(received.len(), 2);
        assert!(received
            .iter()
            .all(|e| e["identity"] == account && e["operation_id"].is_string()));
        assert!(events
            .iter()
            .any(|e| e["event"] == "whatsapp_linked_device"));
        assert!(events.iter().any(|e| e["event"] == "message_duplicate"));
        assert!(!data.to_string().contains("PRIVATE_"));
        assert!(!data.to_string().contains(&mac));
        let operations: Vec<_> = received.iter().map(|e| e["operation_id"].clone()).collect();
        drop(rt);
        let rt = runtime::Runtime::open(dir.path(), 50, 500).unwrap();
        rt.receive_message(&mac_info, &desktop).await;
        assert!(rt.store.next().unwrap().is_none());
        assert_eq!(
            rt.store.participants().unwrap().as_array().unwrap().len(),
            1
        );
        let restored = activity(rt.clone()).await;
        for operation in operations {
            assert!(restored["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["operation_id"] == operation));
        }
    }
}

#[tokio::test]
async fn rejected_ingress_e2e_is_visible_but_never_persisted_as_work() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime::Runtime::open(dir.path(), 50, 500).unwrap();
    let baseline = parsed("100000000000001:7@lid", "REJECT");
    let mut wrong = baseline.clone();
    wrong.source.sender = "100000000000002:7@lid".parse().unwrap();
    let mut own = baseline.clone();
    own.source.is_from_me = true;
    let mut group = baseline.clone();
    group.source.is_group = true;
    let mut offline = baseline.clone();
    offline.is_offline = true;
    let mut disappearing = baseline.clone();
    disappearing.ephemeral_expiration = Some(60);
    let mut status = baseline.clone();
    status.source.chat = "status@broadcast".parse().unwrap();
    for info in [wrong, own, group, offline, disappearing, status] {
        rt.receive_message(&info, &wa::Message::text("REJECTED_CONTENT_CANARY"))
            .await;
    }
    assert!(rt.store.next().unwrap().is_none());
    assert!(rt
        .store
        .participants()
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let data = activity(rt).await;
    let events = data["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "whatsapp_message_ignored")
            .count(),
        6
    );
    assert!(!events.iter().any(|e| e["event"] == "message_received"));
    assert!(!data.to_string().contains("CANARY"));
    assert!(!data.to_string().contains("100000000000002"));
}
