//! Synthetic model/transport journeys with real SQLite authority and immutable Git/LFS bytes.
use super::*;
use crate::{coordination::Route, llm::Llm, permissions::Change, store::Work};
use std::sync::Arc;

fn incoming(s: &Store, identity: &str, text: &str) -> Work {
    s.ingest(identity, &Uuid::new_v4().to_string(), text)
        .unwrap();
    s.next().unwrap().unwrap()
}
fn reviewed(s: &Store, admin: &Work, actor: &str, change: Change) -> String {
    let id = s.propose(actor, change).unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let work = incoming(s, &admin.recipient, &format!("approve {id}"));
    assert!(s
        .handle_control(&work)
        .unwrap()
        .to_lowercase()
        .contains("approved"));
    id
}
struct Fixture {
    _dir: tempfile::TempDir,
    s: Arc<Store>,
    admin: Work,
    a: Work,
    b: Work,
    c: Work,
    scope: String,
}
fn setup(resources: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Store::open(dir.path()).unwrap());
    let admin = incoming(&s, "admin@test", "hi");
    let a = incoming(&s, "a@test", "hi");
    let b = incoming(&s, "b@test", "hi");
    let c = incoming(&s, "c@test", "hi");
    s.bootstrap_admins(std::slice::from_ref(&admin.user))
        .unwrap();
    let ag = reviewed(
        &s,
        &admin,
        &admin.user,
        Change::CreateGroup {
            name: "Participants".into(),
        },
    );
    let bg = reviewed(
        &s,
        &admin,
        &admin.user,
        Change::CreateGroup {
            name: "Responders".into(),
        },
    );
    reviewed(
        &s,
        &admin,
        &admin.user,
        Change::AddMember {
            group: ag.clone(),
            user: a.user.clone(),
        },
    );
    for peer in [&b, &c] {
        reviewed(
            &s,
            &admin,
            &admin.user,
            Change::AddMember {
                group: bg.clone(),
                user: peer.user.clone(),
            },
        );
    }
    let scope = reviewed(
        &s,
        &admin,
        &admin.user,
        Change::CreateCoordinationScope {
            requesters: ag,
            recipients: bg,
            resources,
        },
    );
    Fixture {
        _dir: dir,
        s,
        admin,
        a,
        b,
        c,
        scope,
    }
}
fn start(f: &Fixture, peer: &Work) -> Task {
    let work = incoming(
        &f.s,
        &f.a.recipient,
        "Please coordinate the equipment request",
    );
    f.s.start_conversation(&work, &f.scope, &peer.user).unwrap();
    f.s.finish(&work, "Queued").unwrap();
    f.s.claim_conversation().unwrap().unwrap()
}
fn sent(s: &Store, task: &Task, text: &str) {
    s.prepare_conversation_send(task, text).unwrap();
    s.conversation_result(task.id, "sent").unwrap();
}
fn reply(s: &Store, who: &Work, stanza: &str, text: &str) -> Work {
    s.ingest_reply(
        &who.recipient,
        &Uuid::new_v4().to_string(),
        text,
        true,
        Some(stanza),
    )
    .unwrap();
    s.next().unwrap().unwrap()
}
fn upload(
    s: &Store,
    who: &Work,
    quote: Option<&str>,
    forwarded: bool,
    bytes: &[u8],
    mime: &str,
) -> (Work, String) {
    use sha2::{Digest, Sha256};
    let media = crate::media::MediaRef {
        kind: "document".into(),
        name: "equipment-evidence".into(),
        mime: mime.into(),
        path: "/v/t62/media".into(),
        key: vec![1; 32],
        sha: Sha256::digest(bytes).to_vec(),
        encrypted_sha: vec![2; 32],
        size: bytes.len() as u64,
    };
    s.ingest_media_reply(
        &who.recipient,
        &Uuid::new_v4().to_string(),
        "Please use this evidence",
        &media,
        !forwarded,
        quote,
    )
    .unwrap();
    let work = s.next().unwrap().unwrap();
    let id = s.save_attachment(&work, &media, bytes).unwrap();
    (work, id)
}
fn grant(f: &Fixture, resource: &str, user: &str) {
    reviewed(
        &f.s,
        &f.admin,
        &f.a.user,
        Change::GrantRead {
            resource: resource.into(),
            audience: permissions::Audience::User { id: user.into() },
        },
    );
}

#[test]
fn shared_context_requires_both_acls_including_derived_sources() {
    let f = setup(true);
    let secret =
        f.s.save_note(&f.a.user, "equipment private", "PRIVATE_A_CANARY")
            .unwrap();
    let shared =
        f.s.save_note(&f.a.user, "equipment shared", "SHARED_POLICY_CANARY")
            .unwrap();
    grant(&f, &shared, &f.b.user);
    f.s.read_note(&f.a.user, &secret).unwrap();
    let derived =
        f.s.save_note(&f.a.user, "equipment derived", "DERIVED_PRIVATE_CANARY")
            .unwrap();
    // Simulate a legacy direct grant: source checks must still block this derived note.
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO read_grants VALUES(?,'user',?)",
            params![derived, f.b.user],
        )
        .unwrap();
    let task = start(&f, &f.b);
    let found =
        f.s.conversation_search(&task, "equipment")
            .unwrap()
            .to_string();
    assert!(found.contains(&shared));
    assert!(!found.contains(&secret));
    assert!(!found.contains(&derived));
    assert!(f.s.conversation_read(&task, &secret).is_err());
    assert!(f.s.conversation_binary(&task, &derived).is_err());
    assert!(f
        .s
        .conversation_read(&task, &shared)
        .unwrap()
        .to_string()
        .contains("SHARED_POLICY_CANARY"));
    assert!(f.s.conversation_queue_file(&task, &secret).is_err());
}
#[test]
fn fresh_upload_is_pair_local_durable_and_never_a_global_grant() {
    let f = setup(true);
    let task = start(&f, &f.b);
    sent(&f.s, &task, "Please reply with the evidence");
    let (work, resource) = upload(
        &f.s,
        &f.b,
        Some(&task.stanza),
        false,
        b"PAIR_UPLOAD_CANARY",
        "text/plain",
    );
    assert!(matches!(
        f.s.route_conversation(&work).unwrap(),
        Route::Queued
    ));
    assert!(f.s.read_note(&f.a.user, &resource).is_err());
    assert_eq!(
        f.s.read_note(&f.b.user, &resource).unwrap(),
        "PAIR_UPLOAD_CANARY"
    );
    let response = f.s.claim_conversation().unwrap().unwrap();
    assert!(response.resources);
    assert!(response.messages.to_string_for_test().contains(&resource));
    assert!(f
        .s
        .conversation_read(&response, &resource)
        .unwrap()
        .to_string()
        .contains("PAIR_UPLOAD_CANARY"));
    assert!(f.s.claim_conversation_file().unwrap().is_none());
    f.s.conversation_queue_file(&response, &resource).unwrap(); // deduplicates the automatically staged file
    sent(
        &f.s,
        &response,
        "The participant supplied evidence; the file follows.",
    );
    let file = f.s.claim_conversation_file().unwrap().unwrap();
    assert_eq!(file.recipient, f.a.recipient);
    assert_eq!(
        f.s.conversation_file_bytes(&file).unwrap().2,
        b"PAIR_UPLOAD_CANARY"
    );
    f.s.prepare_conversation_file(&file).unwrap();
    f.s.conversation_file_result(file.id, "sent").unwrap();
    assert!(f.s.claim_conversation_file().unwrap().is_none());
    let other = start(&f, &f.c);
    assert!(!f
        .s
        .conversation_search(&other, "equipment")
        .unwrap()
        .to_string()
        .contains(&resource));
    assert!(f.s.conversation_read(&other, &resource).is_err());
    let stop = reply(&f.s, &f.a, &file.stanza, "sToP");
    assert!(matches!(
        f.s.route_conversation(&stop).unwrap(),
        Route::Reply("Conversation closed.")
    ));
    assert!(!readable(&f.s.db.lock().unwrap(), &response.conversation, &resource).unwrap());
}
trait JsonVec {
    fn to_string_for_test(&self) -> String;
}
impl JsonVec for Vec<Value> {
    fn to_string_for_test(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}
#[test]
fn private_forwarded_wrong_recipient_and_text_only_uploads_never_gain_pair_access() {
    for mode in ["private", "forwarded", "wrong", "text-only"] {
        let f = setup(mode != "text-only");
        let task = start(&f, &f.b);
        sent(&f.s, &task, "Question");
        let who = if mode == "wrong" { &f.c } else { &f.b };
        let (work, resource) = upload(
            &f.s,
            who,
            if mode == "private" {
                None
            } else {
                Some(&task.stanza)
            },
            mode == "forwarded",
            b"UNSHARED_UPLOAD",
            "text/plain",
        );
        let route = f.s.route_conversation(&work).unwrap();
        assert!(if mode == "private" {
            matches!(route, Route::Private)
        } else {
            matches!(route, Route::Reply(_))
        });
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM conversation_uploads", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(f.s.read_note(&f.a.user, &resource).is_err());
        assert!(f.s.claim_conversation_file().unwrap().is_none());
    }
}
#[test]
fn enabling_resources_is_reviewed_and_does_not_upgrade_existing_pairs() {
    let f = setup(false);
    let task = start(&f, &f.b);
    sent(&f.s, &task, "old text-only pair");
    assert!(!task.resources);
    assert!(f
        .s
        .propose(
            &f.a.user,
            Change::SetCoordinationResources {
                scope: f.scope.clone(),
                enabled: true
            }
        )
        .is_err());
    let pending =
        f.s.propose(
            &f.admin.user,
            Change::SetCoordinationResources {
                scope: f.scope.clone(),
                enabled: true,
            },
        )
        .unwrap();
    assert!(!enabled(&f.s.db.lock().unwrap(), &task.conversation).unwrap());
    let approval = reply(
        &f.s,
        &f.admin,
        "not-a-notice",
        &format!("approve {}", pending["request_id"].as_str().unwrap()),
    );
    assert!(f
        .s
        .handle_control(&approval)
        .unwrap()
        .to_lowercase()
        .contains("approved"));
    assert!(!coordination::valid(&f.s.db.lock().unwrap(), &task.conversation).unwrap());
    assert!(start(&f, &f.b).resources);
}
#[test]
fn revocation_drops_current_output_and_excludes_old_resource_derived_drafts() {
    let f = setup(true);
    let shared =
        f.s.save_note(&f.a.user, "equipment rules", "REVOKED_RESOURCE_CANARY")
            .unwrap();
    grant(&f, &shared, &f.b.user);
    let task = start(&f, &f.b);
    f.s.conversation_read(&task, &shared).unwrap();
    sent(&f.s, &task, "REVOKED_RESOURCE_CANARY");
    reviewed(
        &f.s,
        &f.admin,
        &f.a.user,
        Change::RevokeRead {
            resource: shared.clone(),
            audience: permissions::Audience::User {
                id: f.b.user.clone(),
            },
        },
    );
    let work = reply(&f.s, &f.b, &task.stanza, "What is available now?");
    assert!(matches!(
        f.s.route_conversation(&work).unwrap(),
        Route::Queued
    ));
    let next = f.s.claim_conversation().unwrap().unwrap();
    assert!(!next
        .messages
        .to_string_for_test()
        .contains("REVOKED_RESOURCE_CANARY"));
    assert!(f.s.conversation_read(&next, &shared).is_err());
    // Direct ACL changes are checked too, even without an epoch increment.
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO read_grants VALUES(?,'user',?)",
            params![shared, f.b.user],
        )
        .unwrap();
    f.s.conversation_read(&next, &shared).unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM read_grants WHERE resource=?", [&shared])
        .unwrap();
    assert!(f
        .s
        .prepare_conversation_send(&next, "stale result")
        .is_err());
}
#[test]
fn delivery_rechecks_after_upload_and_restart_never_replays_uncertain_files() {
    let f = setup(true);
    let task = start(&f, &f.b);
    sent(&f.s, &task, "Question");
    let (work, resource) = upload(
        &f.s,
        &f.b,
        Some(&task.stanza),
        false,
        b"fresh",
        "text/plain",
    );
    assert!(matches!(
        f.s.route_conversation(&work).unwrap(),
        Route::Queued
    ));
    let next = f.s.claim_conversation().unwrap().unwrap();
    sent(&f.s, &next, "File follows");
    let file = f.s.claim_conversation_file().unwrap().unwrap();
    f.s.conversation_file_bytes(&file).unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE conversations SET expires=0 WHERE id=?",
            [&task.conversation],
        )
        .unwrap();
    assert!(f.s.prepare_conversation_file(&file).is_err());
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE conversations SET expires=unixepoch()+3600 WHERE id=?",
            [&task.conversation],
        )
        .unwrap();
    f.s.prepare_conversation_file(&file).unwrap();
    let path = f._dir.path().to_path_buf();
    let Fixture { _dir, s, .. } = f;
    drop(s);
    let s = Store::open(&path).unwrap();
    assert!(s.claim_conversation_file().unwrap().is_none());
    assert_eq!(
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT state FROM conversation_files WHERE id=?",
                [file.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "unknown"
    );
    assert!(s.read_binary(&work.user, &resource).is_ok());
    drop(s);
    drop(_dir);
}
#[test]
fn file_budgets_are_shared_with_private_replies_and_failed_admission_rolls_back_grants() {
    let f = setup(true);
    let task = start(&f, &f.b);
    sent(&f.s, &task, "Question");
    let private = incoming(&f.s, &f.b.recipient, "send a file");
    let own = f.s.save_note(&f.b.user, "private", "budget").unwrap();
    f.s.queue_file(&private, &own).unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute("UPDATE file_outbox SET size=32*1024*1024", [])
        .unwrap();
    let (work, resource) = upload(
        &f.s,
        &f.b,
        Some(&task.stanza),
        false,
        b"too much",
        "text/plain",
    );
    assert!(matches!(
        f.s.route_conversation(&work).unwrap(),
        Route::Reply(_)
    ));
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversation_uploads WHERE resource=?",
                [resource],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert!(f.s.claim_conversation_file().unwrap().is_none());
}
#[tokio::test]
async fn mock_model_e2e_shared_search_read_reject_private_file_and_deliver_allowed_file() {
    use axum::{routing::post, Json, Router};
    let f = setup(true);
    let shared =
        f.s.save_note(&f.a.user, "equipment rules", "SHARED_RULES_CANARY")
            .unwrap();
    grant(&f, &shared, &f.b.user);
    let secret =
        f.s.save_note(&f.a.user, "PRIVATE_TITLE_CANARY", "PRIVATE_BODY_CANARY")
            .unwrap();
    incoming(&f.s, &f.a.recipient, "PRIVATE_HISTORY_CANARY");
    let task = start(&f, &f.b);
    let resource = shared.clone();
    let app=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>| {
        let resource=resource.clone();let secret=secret.clone();async move {
            let serialized=body.to_string();assert!(!serialized.contains("PRIVATE_BODY_CANARY"));assert!(!serialized.contains("PRIVATE_HISTORY_CANARY"));assert!(!serialized.contains("PRIVATE_TITLE_CANARY"));
            let last=body["messages"].as_array().unwrap().last().unwrap();
            let calls=if last["role"]!="tool" {vec![("search","search_resources",json!({"query":"equipment"}))]}
            else if last["tool_call_id"]=="search" {assert!(last["content"].as_str().unwrap().contains(&resource));vec![("read","read_resource",json!({"id":resource})),("denied","send_file",json!({"id":secret}))]}
            else if last["tool_call_id"]=="denied" {assert!(last["content"].as_str().unwrap().contains("error"));vec![("send","send_file",json!({"id":resource}))]}
            else {return Json(json!({"choices":[{"message":{"role":"assistant","content":"The shared equipment rules apply; the file follows."}}]}))};
            Json(json!({"choices":[{"message":{"role":"assistant","tool_calls":calls.into_iter().map(|(id,name,args)|json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect::<Vec<_>>()}}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let llm = Llm::new(format!("http://{address}/v1"), "test".into()).unwrap();
    let draft = llm.conversation_draft(&f.s, &task, "test").await.unwrap();
    sent(&f.s, &task, &draft);
    let file = f.s.claim_conversation_file().unwrap().unwrap();
    assert_eq!(file.resource, shared);
    assert_eq!(file.recipient, f.b.recipient);
    assert_eq!(
        f.s.conversation_file_bytes(&file).unwrap().2,
        b"SHARED_RULES_CANARY"
    );
    f.s.prepare_conversation_file(&file).unwrap();
    f.s.conversation_file_result(file.id, "sent").unwrap();
    assert!(f.s.claim_conversation_file().unwrap().is_none());
    server.abort();
}
#[tokio::test]
async fn provider_analysis_rechecks_pair_access_after_async_response() {
    use axum::{routing::post, Json, Router};
    let f = setup(true);
    let first = start(&f, &f.b);
    sent(&f.s, &first, "Photo please");
    let (work, resource) = upload(
        &f.s,
        &f.b,
        Some(&first.stanza),
        false,
        b"\x89PNG\r\n\x1a\nsynthetic-image",
        "image/png",
    );
    assert!(matches!(
        f.s.route_conversation(&work).unwrap(),
        Route::Queued
    ));
    let task = f.s.claim_conversation().unwrap().unwrap();
    let store = f.s.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let store = store.clone();
            async move {
                assert!(body.get("tools").is_none());
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute("UPDATE coordination_scopes SET enabled=0", [])
                    .unwrap();
                Json(json!({"choices":[{"message":{"content":"STALE_IMAGE_DESCRIPTION"}}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let llm = Llm::new(format!("http://{address}/v1"), "test".into()).unwrap();
    let call = json!({"function":{"arguments":json!({"id":resource,"question":"Describe this evidence"}).to_string()}});
    assert!(llm
        .analyze_image_for(&f.s, MediaAccess::Scoped(&task), "test", &call)
        .await
        .is_err());
    assert!(f
        .s
        .prepare_conversation_send(&task, "STALE_IMAGE_DESCRIPTION")
        .is_err());
    assert!(f.s.claim_conversation_file().unwrap().is_none());
    server.abort();
}
#[test]
fn native_media_quotes_and_forwarding_flags_are_preserved() {
    let context = wa::ContextInfo {
        stanza_id: Some("EXACT_FILE_STANZA".into()),
        is_forwarded: Some(true),
        ..Default::default()
    };
    let messages = [
        wa::Message {
            image_message: Some(wa::message::ImageMessage {
                context_info: Some(context.clone()).into(),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        },
        wa::Message {
            document_message: Some(wa::message::DocumentMessage {
                context_info: Some(context.clone()).into(),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        },
        wa::Message {
            audio_message: Some(wa::message::AudioMessage {
                context_info: Some(context.clone()).into(),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        },
        wa::Message {
            video_message: Some(wa::message::VideoMessage {
                context_info: Some(context).into(),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        },
    ];
    for message in messages {
        let (quote, allowed) = crate::media::reply_context(&message);
        assert_eq!(quote.as_deref(), Some("EXACT_FILE_STANZA"));
        assert!(!allowed);
    }
}

#[tokio::test]
async fn scoped_pdf_and_audio_use_provider_only_and_preserve_pair_provenance() {
    use axum::{
        body::Bytes,
        http::StatusCode,
        routing::{delete, get, post},
        Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (model, audio, bytes, mime, tool) in [
        (
            "xai/grok-4.7",
            false,
            include_bytes!("../fixtures/pages.pdf").as_slice(),
            "application/pdf",
            "analyze_document",
        ),
        (
            "xai/grok-4.7",
            true,
            b"OggSsynthetic".as_slice(),
            "audio/ogg",
            "transcribe_audio",
        ),
        (
            "codex/test-vision",
            false,
            include_bytes!("../fixtures/pages.pdf").as_slice(),
            "application/pdf",
            "analyze_document",
        ),
        (
            "openai/test-vision",
            false,
            include_bytes!("../fixtures/pages.pdf").as_slice(),
            "application/pdf",
            "analyze_document",
        ),
    ] {
        let f = setup(true);
        let first = start(&f, &f.b);
        sent(&f.s, &first, "Please supply evidence");
        let (work, resource) = upload(&f.s, &f.b, Some(&first.stanza), false, bytes, mime);
        assert!(matches!(
            f.s.route_conversation(&work).unwrap(),
            Route::Queued
        ));
        let task = f.s.claim_conversation().unwrap().unwrap();
        let target = resource.clone();
        let deletes = Arc::new(AtomicUsize::new(0));
        let seen = deletes.clone();
        let app=Router::new()
            .route("/v1/models",get(||async{Json(json!({"data":[{"id":"xai/grok-4.7"}]}))}))
            .route("/v1/files",post(|body:Bytes|async move{let text=String::from_utf8_lossy(&body);assert!(text.contains("%PDF-"));assert!(text.contains("expires_after"));Json(json!({"id":"file_pair","expires_at":9999999999_i64}))}))
            .route("/v1/files/file_pair",delete(move ||{let seen=seen.clone();async move{seen.fetch_add(1,Ordering::SeqCst);StatusCode::NO_CONTENT}}))
            .route("/v1/responses",post(|Json(body):Json<Value>|async move{assert!(body.get("tools").is_none());assert_eq!(body["store"],false);assert_eq!(body["input"][0]["content"][1]["file_id"],"file_pair");Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"PAIR_PROVIDER_RESULT"}]}]}))}))
            .route("/v1/stt",post(|body:Bytes|async move{assert!(String::from_utf8_lossy(&body).contains("OggSsynthetic"));Json(json!({"text":"PAIR_PROVIDER_RESULT"}))}))
            .route("/v1/chat/completions",post(move |Json(body):Json<Value>|{let target=target.clone();async move{
                let last=body["messages"].as_array().unwrap().last().unwrap();
                if last["content"].is_array() {
                    assert_eq!(body["model"],model);assert_eq!(body["store"],false);assert!(body.get("tools").is_none());
                    assert!(last["content"][1]["file"]["file_data"].as_str().unwrap().starts_with("data:application/pdf;base64,"));
                    Json(json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"PAIR_PROVIDER_RESULT"}}]}))
                } else if last["role"]=="tool" {assert!(last["content"].as_str().unwrap().contains("PAIR_PROVIDER_RESULT"));Json(json!({"choices":[{"message":{"content":"PAIR_PROVIDER_RESULT"}}]}))}
                else {Json(json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"parse","type":"function","function":{"name":tool,"arguments":json!({"id":target,"question":"Summarize this evidence"}).to_string()}}]}}]}))}
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let llm = Llm::new(format!("http://{address}/v1"), "test".into()).unwrap();
        let draft = llm.conversation_draft(&f.s, &task, model).await.unwrap();
        assert_eq!(draft, "PAIR_PROVIDER_RESULT");
        assert_eq!(
            deletes.load(Ordering::SeqCst),
            usize::from(!audio && model.starts_with("xai/"))
        );
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM provider_files", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM conversation_sources WHERE task=? AND resource=?",
                params![task.id, resource],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM context_sources WHERE user_id=? AND resource=?",
                params![f.a.user, resource],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        sent(&f.s, &task, &draft);
        assert!(f.s.claim_conversation_file().unwrap().is_some());
        assert!(f.s.read_binary(&f.a.user, &resource).is_err());
        server.abort();
    }
}

#[test]
fn progress_notice_quotes_continue_only_the_exact_pair_and_allow_stop() {
    let f = setup(true);
    let task = start(&f, &f.b);
    let notice =
        f.s.reserve_progress(&crate::progress::Target::pair(&task))
            .unwrap()
            .unwrap();
    assert!(f
        .s
        .reserve_progress(&crate::progress::Target::pair(&task))
        .unwrap()
        .is_none());
    let wrong = reply(&f.s, &f.c, &notice, "wrong person");
    assert!(matches!(
        f.s.route_conversation(&wrong).unwrap(),
        Route::Reply(_)
    ));
    let work = reply(&f.s, &f.b, &notice, "Additional detail while waiting");
    assert!(matches!(
        f.s.route_conversation(&work).unwrap(),
        Route::Queued
    ));
    let stop = reply(&f.s, &f.b, &notice, "STOP");
    assert!(matches!(
        f.s.route_conversation(&stop).unwrap(),
        Route::Reply("Conversation closed.")
    ));
    assert!(f
        .s
        .reserve_progress(&crate::progress::Target::pair(&task))
        .is_err());
    assert!(f.s.prepare_conversation_send(&task, "late draft").is_err());
}

#[test]
fn live_stop_cancels_model_work_before_worker_dequeues_the_command() {
    let f = setup(true);
    let task = start(&f, &f.b);
    let notice =
        f.s.reserve_progress(&crate::progress::Target::pair(&task))
            .unwrap()
            .unwrap();
    for (peer, forwarded) in [(&f.c, false), (&f.b, true)] {
        let id = Uuid::new_v4().to_string();
        f.s.ingest_reply(&peer.recipient, &id, "STOP", !forwarded, Some(&notice))
            .unwrap();
        assert!(!f.s.handle_incoming_stop(&peer.recipient, &id).unwrap());
        assert!(f.s.check_conversation(&task).is_ok());
    }
    let id = Uuid::new_v4().to_string();
    f.s.ingest_reply(&f.b.recipient, &id, "sToP", true, Some(&notice))
        .unwrap();
    assert!(f.s.handle_incoming_stop(&f.b.recipient, &id).unwrap());
    assert!(!f.s.handle_incoming_stop(&f.b.recipient, &id).unwrap());
    assert!(f.s.check_conversation(&task).is_err());
    assert!(f.s.prepare_conversation_send(&task, "late draft").is_err());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM conversation_tasks WHERE id=?",
                [task.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "cancelled"
    );
}
