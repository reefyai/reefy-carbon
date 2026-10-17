//! Best-effort presence and one durable, fixed progress notice. No model-supplied recipients/text.
use crate::{
    permissions,
    runtime::Runtime,
    store::{Store, Work},
};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection};
use std::{
    future::Future,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    sync::oneshot,
    time::{Instant, MissedTickBehavior},
};
use whatsapp_rust::prelude::*;

pub const NOTICE: &str = "Still working on it…";
pub const TURN_TIMEOUT: Duration = Duration::from_secs(600);

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS progress_notices(inbox_id INTEGER NOT NULL REFERENCES inbox(id),task INTEGER REFERENCES conversation_tasks(id),recipient TEXT NOT NULL,stanza TEXT NOT NULL UNIQUE,state TEXT NOT NULL DEFAULT 'unknown',PRIMARY KEY(inbox_id,recipient));
    CREATE VIEW IF NOT EXISTS conversation_deliveries AS SELECT conversation,destination,stanza,state FROM conversation_tasks UNION ALL SELECT t.conversation,t.destination,f.stanza,f.state FROM conversation_files f JOIN conversation_tasks t ON t.id=f.task UNION ALL SELECT t.conversation,t.destination,p.stanza,p.state FROM progress_notices p JOIN conversation_tasks t ON t.id=p.task;")?;
    Ok(())
}
#[derive(Clone)]
pub enum Target {
    Private(Work),
    Pair {
        task: i64,
        conversation: String,
        inbox: i64,
        recipient: String,
    },
}
impl Target {
    pub fn pair(t: &crate::coordination::Task) -> Self {
        Self::Pair {
            task: t.id,
            conversation: t.conversation.clone(),
            inbox: t.inbox,
            recipient: t.recipient.clone(),
        }
    }
    fn details(&self) -> (i64, Option<i64>, &str) {
        match self {
            Self::Private(w) => (w.id, None, &w.recipient),
            Self::Pair {
                task,
                inbox,
                recipient,
                ..
            } => (*inbox, Some(*task), recipient),
        }
    }
    fn check(&self, db: &Connection) -> Result<()> {
        match self {
            Self::Private(w)=>ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id JOIN work_epochs e ON e.inbox_id=i.id WHERE i.id=? AND i.recipient=? AND i.user_id=? AND i.state='processing' AND e.epoch=?)",params![w.id,w.recipient,w.user,permissions::epoch(db)?],|r|r.get::<_,bool>(0))?,"Work unavailable"),
            Self::Pair{task,conversation,inbox,recipient}=>{
                ensure!(crate::scoped_resources::check_task(db,*task,"processing")?==*conversation,"Pair unavailable");
                ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_tasks t JOIN identities i ON i.user_id=t.destination WHERE t.id=? AND t.inbox_id=? AND i.identity=?)",params![task,inbox,recipient],|r|r.get::<_,bool>(0))?,"Destination changed");
            }
        }
        Ok(())
    }
}
impl Store {
    /// Quoted STOP must not wait behind the very model call it cancels.
    pub(crate) fn handle_incoming_stop(&self, identity: &str, provider: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let _authority = self.authority.lock().unwrap();
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        let row=db.query_row("SELECT i.id,i.user_id,t.conversation FROM inbox i JOIN identities a ON a.identity=i.recipient AND a.user_id=i.user_id JOIN approval_quotes q ON q.inbox_id=i.id JOIN conversation_deliveries t ON t.stanza=q.stanza_id AND t.destination=i.user_id JOIN conversations c ON c.id=t.conversation WHERE i.recipient=? AND i.provider_id=? AND i.state='pending' AND lower(trim(i.text))='stop' AND t.state IN('sent','unknown') AND c.state='open' AND ((c.a=i.user_id AND c.a_identity=i.recipient) OR (c.b=i.user_id AND c.b_identity=i.recipient)) AND NOT EXISTS(SELECT 1 FROM blocked_controls WHERE inbox_id=i.id)",params![identity,provider],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
        let Some((inbox, user, conversation)) = row else {
            return Ok(false);
        };
        db.execute(
            "UPDATE conversations SET state='closed' WHERE id=?",
            [&conversation],
        )?;
        db.execute("UPDATE conversation_tasks SET state='cancelled' WHERE conversation=? AND state IN('staged','queued','processing')",[&conversation])?;
        db.execute("UPDATE conversation_files SET state='cancelled' WHERE task IN(SELECT id FROM conversation_tasks WHERE conversation=?) AND state IN('staged','pending','uploading')",[conversation])?;
        db.execute(
            "INSERT OR REPLACE INTO work_epochs VALUES(?,?)",
            params![inbox, permissions::epoch(&db)?],
        )?;
        db.execute(
            "INSERT INTO messages(user_id,role,text) VALUES(?,'assistant','Conversation closed.')",
            [user],
        )?;
        db.execute(
            "INSERT INTO outbox(inbox_id,recipient,text) VALUES(?,?,'Conversation closed.')",
            params![inbox, identity],
        )?;
        db.execute("UPDATE inbox SET state='done' WHERE id=?", [inbox])?;
        db.commit()?;
        drop(guard);
        self.log_work(
            inbox,
            "info",
            "conversation_stopped",
            Some(identity),
            "Quoted STOP cancelled the conversation without waiting for model work",
        );
        Ok(true)
    }
    pub(crate) fn reserve_progress(&self, target: &Target) -> Result<Option<String>> {
        let _authority = self.authority.lock().unwrap();
        let mut guard = self.db.lock().unwrap();
        let db = guard.transaction()?;
        target.check(&db)?;
        let (inbox, task, recipient) = target.details();
        let stanza = uuid::Uuid::new_v4().simple().to_string();
        // Reserve as unknown before network I/O: cancellation/crash must never repeat this notice.
        let changed=db.execute("INSERT OR IGNORE INTO progress_notices(inbox_id,task,recipient,stanza) VALUES(?,?,?,?)",params![inbox,task,recipient,stanza])?;
        db.commit()?;
        Ok((changed == 1).then_some(stanza))
    }
}

pub(crate) trait Feedback: Send + Sync + 'static {
    fn current(&self) -> bool;
    fn allowed(&self) -> bool;
    fn presence(&self, typing: bool) -> impl Future<Output = Result<()>> + Send;
    fn notice(&self) -> impl Future<Output = Result<()>> + Send;
}
pub struct WhatsApp {
    runtime: Arc<Runtime>,
    client: Arc<Client>,
    target: Target,
    epoch: u64,
    sequence: u64,
}
impl WhatsApp {
    pub fn new(runtime: Arc<Runtime>, client: Arc<Client>, target: Target) -> Self {
        let epoch = runtime.transport_epoch.load(Ordering::SeqCst);
        let sequence = runtime.progress_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            runtime,
            client,
            target,
            epoch,
            sequence,
        }
    }
}
impl Feedback for WhatsApp {
    fn current(&self) -> bool {
        self.runtime.transport_epoch.load(Ordering::SeqCst) == self.epoch
            && self.runtime.progress_sequence.load(Ordering::SeqCst) == self.sequence
    }
    fn allowed(&self) -> bool {
        self.current()
            && !self.runtime.unlinking.load(Ordering::SeqCst)
            && self
                .target
                .check(&self.runtime.store.db.lock().unwrap())
                .is_ok()
    }
    async fn presence(&self, typing: bool) -> Result<()> {
        let jid: Jid = self.target.details().2.parse()?;
        if typing {
            self.client.chatstate().send_composing(&jid).await?;
        } else {
            self.client.chatstate().send_paused(&jid).await?;
        }
        Ok(())
    }
    async fn notice(&self) -> Result<()> {
        if !self.allowed() {
            return Ok(());
        }
        let Some(stanza) = self.runtime.store.reserve_progress(&self.target)? else {
            return Ok(());
        };
        let (inbox, _, recipient) = self.target.details();
        self.runtime.store.log_work(
            inbox,
            "info",
            "processing_update_sending",
            Some(recipient),
            "Sending one slow-processing update",
        );
        let result = self
            .client
            .send_message_with_options(
                recipient.parse::<Jid>()?,
                wa::Message::text(NOTICE),
                whatsapp_rust::send::SendOptions::default().with_message_id(stanza.clone()),
            )
            .await;
        if result.is_ok() {
            self.runtime.store.db.lock().unwrap().execute(
                "UPDATE progress_notices SET state='sent' WHERE stanza=?",
                [stanza],
            )?;
        }
        self.runtime.store.log_work(
            inbox,
            if result.is_ok() { "info" } else { "warn" },
            "processing_update",
            Some(recipient),
            if result.is_ok() {
                "Slow-processing update sent"
            } else {
                "Progress update delivery uncertain; not retried"
            },
        );
        Ok(())
    }
}
struct Stop(Option<oneshot::Sender<()>>);
impl Drop for Stop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}
async fn monitor<F: Feedback>(
    feedback: F,
    mut stop: oneshot::Receiver<()>,
    cancelled: oneshot::Sender<()>,
) {
    let mut cancelled = Some(cancelled);
    let started = Instant::now();
    let mut next_typing = started;
    let mut notified = false;
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {biased; _=&mut stop=>break, _=ticks.tick()=>{}}
        if !feedback.allowed() {
            // Cancel model work before waiting for best-effort paused delivery.
            if let Some(tx) = cancelled.take() {
                let _ = tx.send(());
            }
            break;
        }
        let now = Instant::now();
        if now >= next_typing {
            tokio::select! {biased; _=&mut stop=>break, _=tokio::time::timeout(Duration::from_secs(2),feedback.presence(true))=>{}}
            next_typing = Instant::now() + Duration::from_secs(10);
        }
        if !notified && started.elapsed() >= Duration::from_secs(25) {
            notified = true;
            // The model future keeps progressing concurrently; failed feedback never fails the reply.
            tokio::select! {biased; _=&mut stop=>break, _=tokio::time::timeout(Duration::from_secs(5),feedback.notice())=>{}}
        }
    }
    // A disconnected socket cannot be cleared; stop refreshing and let WhatsApp expire presence.
    // A stale guard must not clear a newer operation's typing state.
    if feedback.current() {
        let _ = tokio::time::timeout(Duration::from_secs(2), feedback.presence(false)).await;
    }
}
pub(crate) async fn during<F: Feedback, T>(
    feedback: F,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    let (tx, rx) = oneshot::channel();
    let stop = Stop(Some(tx));
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let mut task = tokio::spawn(monitor(feedback, rx, cancel_tx));
    let result = tokio::select! { biased;
        result=tokio::time::timeout(TURN_TIMEOUT,work)=>result.map_err(|_|anyhow::anyhow!("Processing deadline exceeded")).and_then(|r|r),
        _=cancel_rx=>Err(anyhow::anyhow!("Processing cancelled or disconnected")),
        _=&mut task=>return Err(anyhow::anyhow!("Feedback task interrupted")),
    };
    drop(stop);
    let _ = task.await;
    result
}

#[cfg(test)]
#[path = "../tests/app/progress.rs"]
mod tests;
