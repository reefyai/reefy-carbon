//! Resource access is the intersection of both participants' ACLs, or an exact fresh upload
//! admitted into this pair. Pair upload grants never grant access to the private assistant.
use crate::{
    coordination::{self, Task},
    permissions,
    store::Store,
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;
use whatsapp_rust::prelude::*;

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    let columns = db
        .prepare("PRAGMA table_info(coordination_scopes)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|c| c == "resources") {
        db.execute("ALTER TABLE coordination_scopes ADD COLUMN resources INTEGER NOT NULL DEFAULT 0 CHECK(resources IN(0,1))",[])?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS conversation_uploads(conversation TEXT NOT NULL REFERENCES conversations(id),resource TEXT NOT NULL REFERENCES resources(id),inbox_id INTEGER NOT NULL UNIQUE REFERENCES inbox(id),PRIMARY KEY(conversation,resource));
    CREATE TABLE IF NOT EXISTS conversation_sources(task INTEGER NOT NULL REFERENCES conversation_tasks(id),resource TEXT NOT NULL REFERENCES resources(id),PRIMARY KEY(task,resource));
    CREATE TABLE IF NOT EXISTS conversation_task_epochs(task INTEGER PRIMARY KEY REFERENCES conversation_tasks(id),epoch INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS conversation_files(id INTEGER PRIMARY KEY,task INTEGER NOT NULL REFERENCES conversation_tasks(id),resource TEXT NOT NULL REFERENCES resources(id),size INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'staged',stanza TEXT NOT NULL UNIQUE,created INTEGER NOT NULL DEFAULT(unixepoch()),UNIQUE(task,resource));
    CREATE INDEX IF NOT EXISTS conversation_file_state ON conversation_files(state,id);
    CREATE INDEX IF NOT EXISTS conversation_file_created ON conversation_files(created);
    UPDATE conversation_files SET state='unknown' WHERE state='sending';
    UPDATE conversation_files SET state='failed' WHERE state='uploading';
    UPDATE conversation_files SET state='cancelled' WHERE state='staged';")?;
    Ok(())
}
pub(crate) fn enabled(db: &Connection, conversation: &str) -> Result<bool> {
    Ok(db.query_row("SELECT s.resources FROM coordination_scopes s JOIN conversations c ON c.scope=s.id WHERE c.id=?",[conversation],|r|r.get(0))?)
}
pub(crate) fn readable(db: &Connection, conversation: &str, resource: &str) -> Result<bool> {
    if !coordination::valid(db, conversation)? || !enabled(db, conversation)? {
        return Ok(false);
    }
    let (a, b): (String, String) = db.query_row(
        "SELECT a,b FROM conversations WHERE id=?",
        [conversation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if permissions::can_read(db, &a, resource)? && permissions::can_read(db, &b, resource)? {
        return Ok(true);
    }
    // Exact fresh upload, owned by the authenticated uploader, with no derived/private sources.
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_uploads u JOIN inbox i ON i.id=u.inbox_id JOIN inbox_media m ON m.inbox_id=i.id AND m.resource=u.resource AND m.state='stored' JOIN resources r ON r.id=u.resource AND r.owner=i.user_id JOIN conversations c ON c.id=u.conversation WHERE u.conversation=? AND u.resource=? AND (i.user_id=c.a OR i.user_id=c.b) AND NOT EXISTS(SELECT 1 FROM blocked_controls WHERE inbox_id=i.id) AND NOT EXISTS(SELECT 1 FROM resource_sources WHERE resource=r.id))",params![conversation,resource],|r|r.get(0))?)
}
pub(crate) fn sources_valid(db: &Connection, task: i64, conversation: &str) -> Result<bool> {
    let mut q = db.prepare("SELECT resource FROM conversation_sources WHERE task=? LIMIT 65")?;
    let ids = q
        .query_map([task], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > 64 {
        return Ok(false);
    }
    for id in ids {
        if !readable(db, conversation, &id)? {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(crate) fn check_task(db: &Connection, task: i64, state: &str) -> Result<String> {
    let conversation:String=db.query_row("SELECT t.conversation FROM conversation_tasks t JOIN conversation_task_epochs e ON e.task=t.id WHERE t.id=? AND t.state=? AND e.epoch=?",params![task,state,permissions::epoch(db)?],|r|r.get(0))?;
    ensure!(
        coordination::valid(db, &conversation)? && sources_valid(db, task, &conversation)?,
        "Conversation access changed"
    );
    Ok(conversation)
}
fn record(db: &Connection, task: i64, resource: &str) -> Result<()> {
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM conversation_sources WHERE task=?",
        [task],
        |r| r.get(0),
    )?;
    ensure!(
        count < 64
            || db.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_sources WHERE task=? AND resource=?)",
                params![task, resource],
                |r| r.get::<_, bool>(0)
            )?,
        "Resource context limit reached"
    );
    db.execute(
        "INSERT OR IGNORE INTO conversation_sources VALUES(?,?)",
        params![task, resource],
    )?;
    Ok(())
}
fn metadata(db: &Connection, task: i64, conversation: &str, id: &str) -> Result<Value> {
    ensure!(
        readable(db, conversation, id)?,
        "Resource unavailable to this pair"
    );
    let value=db.query_row("SELECT r.name,m.kind,m.mime,r.size FROM resources r JOIN resource_meta m ON m.resource=r.id WHERE r.id=?",[id],|r|Ok(json!({"id":id,"name":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"mime":r.get::<_,String>(2)?,"size":r.get::<_,i64>(3)?})))?;
    record(db, task, id)?;
    Ok(value)
}
/// Reserve budgets across private and pair deliveries, including cancelled/failed attempts.
pub(crate) fn outbound_budget(db: &Connection, user: &str, size: i64) -> Result<()> {
    let (personal,total,pc,tc):(i64,i64,i64,i64)=db.query_row("SELECT COALESCE(SUM(CASE WHEN user_id=?1 THEN size ELSE 0 END),0),COALESCE(SUM(size),0),COALESCE(SUM(CASE WHEN user_id=?1 THEN 1 ELSE 0 END),0),COUNT(*) FROM (SELECT i.user_id,f.size FROM file_outbox f JOIN inbox i ON i.id=f.inbox_id WHERE f.created>=unixepoch()-86400 UNION ALL SELECT t.source AS user_id,f.size FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task WHERE f.created>=unixepoch()-86400)",[user],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    ensure!(
        (0..=crate::media::MAX_BYTES as i64).contains(&size)
            && personal + size <= 32 * 1024 * 1024
            && total + size <= 256 * 1024 * 1024
            && pc < 20
            && tc < 200,
        "Outbound limit reached"
    );
    Ok(())
}
pub(crate) fn queue_file(db: &Connection, task: i64, resource: &str) -> Result<Value> {
    let (conversation,source,inbox):(String,String,i64)=db.query_row("SELECT conversation,source,inbox_id FROM conversation_tasks WHERE id=? AND state IN('queued','processing')",[task],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    ensure!(
        readable(db, &conversation, resource)?,
        "Resource unavailable to this pair"
    );
    let data = metadata(db, task, &conversation, resource)?;
    ensure!(data["kind"] != "folder", "Not a sendable file");
    if let Some(state) = db
        .query_row(
            "SELECT state FROM conversation_files WHERE task=? AND resource=?",
            params![task, resource],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(json!({"resource":resource,"state":state}));
    }
    let count:i64=db.query_row("SELECT (SELECT COUNT(*) FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task WHERE t.inbox_id=?1)+(SELECT COUNT(*) FROM file_outbox WHERE inbox_id=?1)",[inbox],|r|r.get(0))?;
    ensure!(count < 3, "At most three files per incoming request");
    let size = data["size"].as_i64().context("Missing size")?;
    outbound_budget(db, &source, size)?;
    db.execute(
        "INSERT INTO conversation_files(task,resource,size,stanza) VALUES(?,?,?,?)",
        params![task, resource, size, Uuid::new_v4().simple().to_string()],
    )?;
    Ok(
        json!({"resource":resource,"state":"staged","notice":"Will be queued only after this text reply is sent. Delivery is not yet confirmed."}),
    )
}
/// Within the routing transaction. Never upgrades an old text-only scope.
pub(crate) fn admit_upload(
    db: &Connection,
    task: i64,
    conversation: &str,
    inbox: i64,
) -> Result<()> {
    let resource: Option<String> = db
        .query_row(
            "SELECT resource FROM inbox_media WHERE inbox_id=? AND state='stored'",
            [inbox],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(resource) = resource {
        ensure!(enabled(db, conversation)?, "This scope is text-only");
        db.execute(
            "INSERT INTO conversation_uploads VALUES(?,?,?)",
            params![conversation, resource, inbox],
        )?;
        queue_file(db, task, &resource)?;
    }
    Ok(())
}
pub(crate) fn current_upload(
    db: &Connection,
    task: i64,
    conversation: &str,
    inbox: i64,
) -> Result<Option<Value>> {
    let id = db
        .query_row(
            "SELECT resource FROM conversation_uploads WHERE conversation=? AND inbox_id=?",
            params![conversation, inbox],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    id.map(|id| metadata(db, task, conversation, &id))
        .transpose()
}

impl Store {
    pub(crate) fn check_conversation(&self, task: &Task) -> Result<()> {
        let db = self.db.lock().unwrap();
        ensure!(
            check_task(&db, task.id, "processing")? == task.conversation,
            "Wrong task context"
        );
        Ok(())
    }
    /// Caller holds the authority guard. No private-history provenance is read or modified.
    pub(crate) fn conversation_binary(
        &self,
        task: &Task,
        id: &str,
    ) -> Result<(String, String, Vec<u8>)> {
        let db = self.db.lock().unwrap();
        ensure!(
            check_task(&db, task.id, "processing")? == task.conversation
                && readable(&db, &task.conversation, id)?,
            "Resource unavailable"
        );
        let result = self.verified_bytes(&db, id)?;
        record(&db, task.id, id)?;
        Ok(result)
    }
    pub(crate) fn conversation_read(&self, task: &Task, id: &str) -> Result<Value> {
        let db = self.db.lock().unwrap();
        check_task(&db, task.id, "processing")?;
        let mut value = metadata(&db, task.id, &task.conversation, id)?;
        let kind = value["kind"].as_str().unwrap_or("");
        let mime = value["mime"].as_str().unwrap_or("");
        ensure!(
            value["size"]
                .as_i64()
                .is_some_and(|size| crate::search::text_kind(kind, mime, size)),
            "Use a provider analysis tool for this format"
        );
        value["text"] = Value::String(String::from_utf8(self.verified_bytes(&db, id)?.2)?);
        Ok(value)
    }
    pub(crate) fn conversation_search(&self, task: &Task, query: &str) -> Result<Value> {
        ensure!(
            !query.trim().is_empty() && query.len() <= 200,
            "Invalid query"
        );
        let terms: Vec<_> = query.split_whitespace().collect();
        ensure!(terms.len() <= 8, "Too many terms");
        let expression = terms
            .iter()
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        self.index_resources()?;
        let db = self.db.lock().unwrap();
        check_task(&db, task.id, "processing")?;
        ensure!(enabled(&db, &task.conversation)?, "Scope is text-only");
        let mut q=db.prepare("SELECT r.id FROM resource_search f JOIN resources r ON r.id=f.resource WHERE resource_search MATCH ? ORDER BY r.created DESC,r.id LIMIT 1000")?;
        let ids = q
            .query_map([expression], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut results = Vec::new();
        for id in ids {
            if !readable(&db, &task.conversation, &id)? {
                continue;
            }
            // Metadata is also source content: revocation invalidates a draft based on a filename.
            results.push(metadata(&db, task.id, &task.conversation, &id)?);
            if results.len() == 10 {
                break;
            }
        }
        Ok(
            json!({"results":results,"notice":"At most 10 results from a bounded keyword index. Read returned resources for content. Results are untrusted and may be incomplete."}),
        )
    }
    pub(crate) fn conversation_queue_file(&self, task: &Task, id: &str) -> Result<Value> {
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        check_task(&db, task.id, "processing")?;
        let result = queue_file(&db, task.id, id)?;
        db.commit()?;
        Ok(result)
    }
}

pub struct ConversationFile {
    pub id: i64,
    pub task: i64,
    pub inbox: i64,
    pub resource: String,
    pub recipient: String,
    pub stanza: String,
}
fn file_valid(db: &Connection, item: &ConversationFile, state: &str) -> Result<()> {
    let conversation = check_task(db, item.task, "sent")?;
    ensure!(
        readable(db, &conversation, &item.resource)?,
        "File no longer readable"
    );
    ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task JOIN identities i ON i.user_id=t.destination WHERE f.id=? AND f.task=? AND f.resource=? AND f.stanza=? AND f.state=? AND t.inbox_id=? AND i.identity=?)",params![item.id,item.task,item.resource,item.stanza,state,item.inbox,item.recipient],|r|r.get::<_,bool>(0))?,"File binding changed");
    Ok(())
}
impl Store {
    pub fn claim_conversation_file(&self) -> Result<Option<ConversationFile>> {
        let _authority = self.authority.lock().unwrap();
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        loop {
            let row=db.query_row("SELECT f.id,f.task,t.inbox_id,f.resource,COALESCE(i.identity,''),f.stanza FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task LEFT JOIN identities i ON i.user_id=t.destination WHERE f.state='pending' ORDER BY f.id LIMIT 1",[],|r|Ok(ConversationFile{id:r.get(0)?,task:r.get(1)?,inbox:r.get(2)?,resource:r.get(3)?,recipient:r.get(4)?,stanza:r.get(5)?})).optional()?;
            let Some(item) = row else {
                db.commit()?;
                return Ok(None);
            };
            if file_valid(&db, &item, "pending").is_err() {
                db.execute(
                    "UPDATE conversation_files SET state='cancelled' WHERE id=?",
                    [item.id],
                )?;
                continue;
            }
            db.execute(
                "UPDATE conversation_files SET state='uploading' WHERE id=?",
                [item.id],
            )?;
            db.commit()?;
            return Ok(Some(item));
        }
    }
    pub(crate) fn conversation_file_bytes(
        &self,
        item: &ConversationFile,
    ) -> Result<(String, String, Vec<u8>)> {
        let db = self.db.lock().unwrap();
        file_valid(&db, item, "uploading")?;
        self.verified_bytes(&db, &item.resource)
    }
    pub(crate) fn prepare_conversation_file(&self, item: &ConversationFile) -> Result<()> {
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        file_valid(&db, item, "uploading")?;
        db.execute(
            "UPDATE conversation_files SET state='sending' WHERE id=?",
            [item.id],
        )?;
        db.commit()?;
        Ok(())
    }
    pub fn conversation_file_result(&self, id: i64, state: &str) -> Result<()> {
        ensure!(
            ["sent", "unknown", "failed", "cancelled"].contains(&state),
            "Invalid file state"
        );
        self.db.lock().unwrap().execute(
            "UPDATE conversation_files SET state=? WHERE id=? AND state IN('uploading','sending')",
            params![state, id],
        )?;
        Ok(())
    }
}
pub async fn deliver(
    store: &Store,
    client: &Client,
    item: &ConversationFile,
) -> Result<&'static str> {
    let (name, kind, bytes) = {
        let _authority = store.authority.lock().unwrap();
        store.conversation_file_bytes(item)?
    };
    let jid: Jid = item.recipient.parse()?;
    let upload = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        client.upload(
            bytes,
            whatsapp_rust::download::MediaType::Document,
            whatsapp_rust::UploadOptions::default(),
        ),
    )
    .await;
    let upload = match upload {
        Ok(Ok(upload)) => upload,
        _ => return Ok("failed"),
    };
    {
        let _authority = store.authority.lock().unwrap();
        if store.prepare_conversation_file(item).is_err() {
            return Ok("cancelled");
        }
    }
    let mut message = crate::delivery::document(upload, &name, &kind);
    let mut document = message
        .document_message
        .as_option()
        .context("Missing file payload")?
        .clone();
    document.caption =
        Some("Reply to this file to continue this conversation. Reply STOP to end.".into());
    message.document_message = document.into();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        client.send_message_with_options(
            jid,
            message,
            whatsapp_rust::send::SendOptions::default().with_message_id(item.stanza.clone()),
        ),
    )
    .await;
    Ok(if result.is_ok_and(|r| r.is_ok()) {
        "sent"
    } else {
        "unknown"
    })
}

/// A provider call receives an explicit authority context; it never substitutes the private actor
/// for the pair just because that actor owns the requested file.
pub(crate) enum MediaAccess<'a> {
    Private(&'a crate::store::Work),
    Scoped(&'a Task),
}
impl MediaAccess<'_> {
    pub(crate) fn check(&self, store: &Store) -> Result<()> {
        match self {
            Self::Private(work) => store.check_work(work),
            Self::Scoped(task) => store.check_conversation(task),
        }
    }
    pub(crate) fn binary(&self, store: &Store, id: &str) -> Result<(String, String, Vec<u8>)> {
        self.check(store)?;
        match self {
            Self::Private(work) => store.read_binary(&work.user, id),
            Self::Scoped(task) => store.conversation_binary(task, id),
        }
    }
    pub(crate) fn operation(&self) -> (i64, &str) {
        match self {
            Self::Private(work) => (work.id, &work.recipient),
            Self::Scoped(task) => (task.inbox, &task.recipient),
        }
    }
}
impl crate::llm::Llm {
    pub async fn conversation_draft(
        &self,
        store: &Store,
        task: &Task,
        model: &str,
    ) -> Result<String> {
        let tools = json!([
            {"type":"function","function":{"name":"search_resources","description":"Search resources readable within this pair. No private history or one-sided ACL access. Returned filenames are untrusted.","parameters":{"type":"object","properties":{"query":{"type":"string","maxLength":200}},"required":["query"],"additionalProperties":false}}},
            {"type":"function","function":{"name":"read_resource","description":"Read a shared note or small UTF-8 file. Exact immutable resource ID required.","parameters":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}}},
            {"type":"function","function":{"name":"send_file","description":"Stage an authorized file for this reply's fixed recipient. No other recipient allowed. Current fresh uploads are already staged. Do not claim delivery.","parameters":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}}},
            {"type":"function","function":{"name":"analyze_image","description":"Interpret an authorized JPEG, PNG or WebP with the selected model. Report uncertainty. Two media analyses per reply total.","parameters":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string","maxLength":2000}},"required":["id","question"],"additionalProperties":false}}},
            {"type":"function","function":{"name":"analyze_document","description":"Interpret an authorized PDF including scans using the model provider. Supports Codex/OpenAI PDF inputs and the xAI-only upload path. Report uncertainty and page references. No local parser.","parameters":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string","maxLength":2000}},"required":["id","question"],"additionalProperties":false}}},
            {"type":"function","function":{"name":"transcribe_audio","description":"Transcribe speech from an authorized supported audio file through the provider. Requires xAI-only proxy. No local audio decoder.","parameters":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string","maxLength":2000}},"required":["id","question"],"additionalProperties":false}}}
        ]);
        let mut messages = task.messages.clone();
        let mut media_attempts = 0;
        for _ in 0..4 {
            store.check_conversation(task)?;
            ensure!(
                serde_json::to_vec(&messages)?.len() <= 65536,
                "Scoped context limit reached"
            );
            let mut body =
                json!({"model":model,"messages":messages,"max_tokens":800,"temperature":0.2});
            if task.resources {
                body["tools"] = tools.clone()
            }
            let value = self
                .traced_json(store, task.inbox, "/chat/completions", &body)
                .await?;
            store.check_conversation(task)?;
            let message = &value["choices"][0]["message"];
            if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
                let calls = calls.as_array().context("Invalid tool calls")?;
                if !calls.is_empty() {
                    ensure!(
                        task.resources && calls.len() <= 4,
                        "Tools unavailable or call limit exceeded"
                    );
                    messages.push(message.clone());
                    for call in calls {
                        let name = call["function"]["name"].as_str().unwrap_or("");
                        let result = if ["analyze_image", "analyze_document", "transcribe_audio"]
                            .contains(&name)
                        {
                            media_attempts += 1;
                            if media_attempts > 2 {
                                Err(anyhow::anyhow!("Media limit reached"))
                            } else if name == "analyze_image" {
                                self.analyze_image_for(
                                    store,
                                    MediaAccess::Scoped(task),
                                    model,
                                    call,
                                )
                                .await
                            } else {
                                self.analyze_attachment_for(
                                    store,
                                    MediaAccess::Scoped(task),
                                    model,
                                    call,
                                    name == "transcribe_audio",
                                )
                                .await
                            }
                        } else {
                            let _authority = store.authority.lock().unwrap();
                            (|| -> Result<Value> {
                                store.check_conversation(task)?;
                                #[derive(serde::Deserialize)]
                                #[serde(deny_unknown_fields)]
                                struct Id {
                                    id: String,
                                }
                                #[derive(serde::Deserialize)]
                                #[serde(deny_unknown_fields)]
                                struct Query {
                                    query: String,
                                }
                                let args = call["function"]["arguments"]
                                    .as_str()
                                    .context("Missing arguments")?;
                                match name {
                                    "search_resources" => store.conversation_search(
                                        task,
                                        &serde_json::from_str::<Query>(args)?.query,
                                    ),
                                    "read_resource" => store.conversation_read(
                                        task,
                                        &serde_json::from_str::<Id>(args)?.id,
                                    ),
                                    "send_file" => store.conversation_queue_file(
                                        task,
                                        &serde_json::from_str::<Id>(args)?.id,
                                    ),
                                    _ => anyhow::bail!("Tool unavailable"),
                                }
                            })()
                        };
                        store.trace(task.inbox,"tool_result",json!({"call":call,"ok":result.is_ok(),"result":match &result {Ok(v)=>v.clone(),Err(e)=>json!({"error":format!("{e:#}")})}}));
                        // Even an error may follow revocation during a provider call. Do not continue
                        // the model with earlier context after any authority/source barrier changed.
                        store.check_conversation(task)?;
                        store.log_work(
                            task.inbox,
                            if result.is_ok() { "info" } else { "warn" },
                            if result.is_ok() {
                                "conversation_resource_used"
                            } else {
                                "conversation_tool_rejected"
                            },
                            Some(&task.recipient),
                            if result.is_ok() {
                                "Authorized pair resource operation completed"
                            } else {
                                "Pair resource operation rejected or failed"
                            },
                        );
                        messages.push(json!({"role":"tool","tool_call_id":call["id"],"content":match result {Ok(value)=>value.to_string(),Err(error)=>json!({"error":crate::attachment_llm::tool_error(&error).unwrap_or("Unavailable resource, invalid tool arguments, unsupported provider/format, or limit reached. Do not claim success. Do not request permissions; use available context or ask factual questions.")}).to_string()}}));
                    }
                    continue;
                }
            }
            let text = message["content"].as_str().context("No draft")?.trim();
            ensure!(
                !text.is_empty() && text.len() <= 4000,
                "Draft outside limits"
            );
            return Ok(text.into());
        }
        anyhow::bail!("Scoped model round limit reached")
    }
}

#[cfg(test)]
#[path = "../tests/app/scoped_resources_e2e.rs"]
mod tests;
