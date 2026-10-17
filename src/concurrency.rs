//! Bounded application processing; SQLite claims serialize participants across lanes.
use crate::{
    coordination::Task,
    store::{Store, Work},
};
use anyhow::{ensure, Result};
use std::{collections::HashMap, future::Future, sync::Arc};
use tokio::task::{Id, JoinSet};

#[derive(Clone)]
pub(crate) enum Job {
    Private(Work),
    Pair(Task),
}
impl Job {
    pub(crate) fn abandon(&self, store: &Store) {
        match self {
            Self::Private(w) => {
                let _authority = store.authority.lock().unwrap();
                let interrupted = {
                    let mut db = store.db.lock().unwrap();
                    let tx = match db.transaction() {
                        Ok(tx) => tx,
                        Err(_) => return,
                    };
                    let changed=tx.execute("UPDATE inbox SET state='interrupted' WHERE id=? AND state='processing'",[w.id]).unwrap_or(0);
                    if changed == 1 {
                        // Missing work authority fences late responses before any tool or finish.
                        let _ = tx.execute("DELETE FROM work_epochs WHERE inbox_id=?", [w.id]);
                    }
                    if tx.commit().is_err() {
                        return;
                    }
                    changed == 1
                };
                if !interrupted {
                    return;
                }
                let _ = store.cancel_task_actions(w);
                let _ = store.cancel_staged_files(w);
                let _ = store.cancel_staged_conversations(w);
                store.log_work(
                    w.id,
                    "warn",
                    "processing_interrupted",
                    Some(&w.recipient),
                    "Incomplete processing stopped; staged actions cancelled",
                );
            }
            Self::Pair(t) => {
                let _authority = store.authority.lock().unwrap();
                let state = store
                    .db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM conversation_tasks WHERE id=?",
                        [t.id],
                        |r| r.get::<_, String>(0),
                    )
                    .unwrap_or_default();
                let _ = store.conversation_result(
                    t.id,
                    if state == "sending" {
                        "unknown"
                    } else {
                        "failed"
                    },
                );
            }
        }
    }
}

pub(crate) struct ProcessingPool {
    store: Arc<Store>,
    jobs: JoinSet<()>,
    claimed: HashMap<Id, Job>,
    limit: usize,
    prefer_pair: bool,
}
impl ProcessingPool {
    pub(crate) fn new(store: Arc<Store>, limit: usize) -> Self {
        assert!((1..=16).contains(&limit));
        Self {
            store,
            jobs: JoinSet::new(),
            claimed: HashMap::new(),
            limit,
            prefer_pair: false,
        }
    }
    pub(crate) fn reap(&mut self) {
        while let Some(result) = self.jobs.try_join_next_with_id() {
            let id = match &result {
                Ok((id, _)) => *id,
                Err(e) => e.id(),
            };
            if let Some(job) = self.claimed.remove(&id) {
                job.abandon(&self.store);
            }
            if result.is_err() {
                self.store.log(
                    "error",
                    "processing_job_failed",
                    None,
                    "Processing job panicked or was cancelled; staged work discarded",
                );
            }
        }
    }
    pub(crate) fn fill<F, Fut>(&mut self, has_model: bool, run: F) -> Result<usize>
    where
        F: Fn(Job) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.reap();
        ensure!(
            self.jobs.len() <= self.limit,
            "Processing pool exceeded its limit"
        );
        let mut count = 0;
        while self.jobs.len() < self.limit {
            // Alternate private and legacy pair work so neither continuously starves the other.
            let pair_first = self.prefer_pair && has_model;
            let mut job = if pair_first {
                self.store.claim_pair()?.map(Job::Pair)
            } else {
                self.store.claim_private(!has_model)?.map(Job::Private)
            };
            if job.is_none() {
                job = if pair_first {
                    self.store.claim_private(!has_model)?.map(Job::Private)
                } else if has_model {
                    self.store.claim_pair()?.map(Job::Pair)
                } else {
                    None
                };
            }
            let Some(job) = job else { break };
            self.prefer_pair = !self.prefer_pair;
            let task = self.jobs.spawn(run(job.clone()));
            self.claimed.insert(task.id(), job);
            count += 1;
        }
        Ok(count)
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.jobs.len()
    }
}
impl Drop for ProcessingPool {
    fn drop(&mut self) {
        self.jobs.abort_all();
        // Fence durable state immediately: late provider responses cannot commit cancelled work.
        for job in self.claimed.values() {
            job.abandon(&self.store);
        }
    }
}
