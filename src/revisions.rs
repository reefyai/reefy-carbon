//! Immutable, owner-only note revisions. A new revision never inherits an audience.
use crate::store::Store;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS note_revisions(resource TEXT PRIMARY KEY REFERENCES resources(id),root TEXT NOT NULL REFERENCES resources(id),previous TEXT UNIQUE REFERENCES resources(id),number INTEGER NOT NULL CHECK(number>0),UNIQUE(root,number));
    INSERT OR IGNORE INTO note_revisions(resource,root,number) SELECT resource,resource,1 FROM resource_meta WHERE kind='note';")?;
    Ok(())
}

/// The exact immutable resource ID is the optimistic concurrency token.
pub(crate) fn base(db: &Connection, user: &str, id: &str) -> Result<(String, i64)> {
    let value: Option<(String,i64)> = db.query_row("SELECT v.root,v.number FROM note_revisions v JOIN resources r ON r.id=v.resource WHERE v.resource=? AND r.owner=?",params![id,user],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let value = value.context("Owned note unavailable")?;
    ensure!(
        crate::permissions::can_read(db, user, id)?,
        "Owned note unavailable"
    );
    Ok(value)
}

pub(crate) fn latest_visible(db: &Connection, user: &str, id: &str) -> Result<bool> {
    let mut q=db.prepare("SELECT newer.resource FROM note_revisions old JOIN note_revisions newer ON newer.root=old.root AND newer.number>old.number WHERE old.resource=? ORDER BY newer.number DESC LIMIT 100")?;
    for row in q.query_map([id], |r| r.get::<_, String>(0))? {
        if crate::permissions::can_read(db, user, &row?)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Preserve all non-lineage sources; an owner's older revisions need not be published
/// merely to share the new one. Their external dependencies still apply transitively.
pub(crate) fn inherit_sources(db: &Connection, resource: &str, root: &str) -> Result<()> {
    db.execute("INSERT OR IGNORE INTO resource_sources(resource,source) SELECT ?1,s.source FROM resource_sources s JOIN note_revisions v ON v.resource=s.resource WHERE v.root=?2 AND s.source NOT IN(SELECT resource FROM note_revisions WHERE root=?2)",params![resource,root])?;
    db.execute("DELETE FROM resource_sources WHERE resource=?1 AND source IN(SELECT resource FROM note_revisions WHERE root=?2)",params![resource,root])?;
    Ok(())
}

impl Store {
    pub fn note_history(&self, user: &str, id: &str) -> Result<Value> {
        let db = self.db.lock().unwrap();
        ensure!(
            crate::permissions::can_read(&db, user, id)?,
            "Note unavailable"
        );
        let root: String = db
            .query_row(
                "SELECT root FROM note_revisions WHERE resource=?",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .context("Note unavailable")?;
        let mut q=db.prepare("SELECT r.id,r.name,r.created FROM note_revisions v JOIN resources r ON r.id=v.resource WHERE v.root=? ORDER BY v.number DESC LIMIT 100")?;
        let candidates = q
            .query_map([root], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut versions = Vec::new();
        for (id, name, created) in candidates {
            if crate::permissions::can_read(&db, user, &id)? {
                db.execute(
                    "INSERT OR IGNORE INTO context_sources VALUES(?,?)",
                    params![user, id],
                )?;
                versions.push(json!({"id":id,"name":name,"created":created}));
            }
        }
        Ok(
            json!({"versions":versions,"notice":"Newest accessible revision first. Each ID names immutable content. Only currently readable revisions are listed; no access to private predecessors is implied. Use read_note for content."}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Audience, Change};
    use std::sync::Arc;
    fn user(s: &Store, identity: &str) -> String {
        s.ingest(identity, "hello", "hi").unwrap();
        s.next().unwrap().unwrap().user
    }
    fn grant(s: &Store, id: &str, user: &str) {
        s.db.lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO read_grants VALUES(?,'user',?)",
                params![id, user],
            )
            .unwrap();
    }
    #[test]
    fn private_versions_are_immutable_and_do_not_inherit_sharing_or_folders() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = user(&s, "owner@test");
        let b = user(&s, "reader@test");
        let c = user(&s, "outsider@test");
        s.bootstrap_admins(std::slice::from_ref(&a)).unwrap();
        let old = s.save_note(&a, "Instructions", "OLD_PRIVATE_TEXT").unwrap();
        let folder = s.create_folder(&a, "Shared folder").unwrap();
        grant(&s, &folder, &b);
        s.db.lock()
            .unwrap()
            .execute(
                "UPDATE resource_meta SET parent=? WHERE resource=?",
                params![folder, old],
            )
            .unwrap();
        assert_eq!(s.read_note(&b, &old).unwrap(), "OLD_PRIVATE_TEXT");
        s.read_note(&a, &old).unwrap();
        let new = s
            .revise_note(&a, &old, "Instructions", "NEW_PRIVATE_TEXT")
            .unwrap();
        assert_eq!(s.read_note(&a, &old).unwrap(), "OLD_PRIVATE_TEXT");
        assert_eq!(s.read_note(&a, &new).unwrap(), "NEW_PRIVATE_TEXT");
        assert!(s.read_note(&b, &new).is_err());
        assert!(s.note_history(&c, &old).is_err());
        assert!(!s.note_history(&b, &old).unwrap().to_string().contains(&new));
        assert!(s.notes(&a).unwrap().to_string().contains(&new));
        assert!(!s.notes(&a).unwrap().to_string().contains(&old));
        assert!(s.notes(&b).unwrap().to_string().contains(&old));
        assert!(
            s.search_resources(&b, "NEW_PRIVATE_TEXT").unwrap()["results"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Publication of the new revision does not require disclosing its predecessor.
        s.propose(
            &a,
            Change::GrantRead {
                resource: new.clone(),
                audience: Audience::User { id: c.clone() },
            },
        )
        .unwrap();
        grant(&s, &new, &c);
        let history = s.note_history(&c, &new).unwrap();
        assert_eq!(history["versions"].as_array().unwrap().len(), 1);
        assert!(!history.to_string().contains(&old));
        assert!(s.read_note(&c, &old).is_err());
        assert!(s
            .revise_note(&b, &old, "Instructions", "overwrite")
            .is_err());
        assert!(s.revise_note(&a, &folder, "Folder", "overwrite").is_err());
    }
    #[test]
    fn stale_and_concurrent_edits_fail_and_immediate_retries_are_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(d.path()).unwrap());
        let a = user(&s, "owner@test");
        let old = s.save_note(&a, "Plan", "v1").unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|n| {
                let (s, a, old, barrier) = (s.clone(), a.clone(), old.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    s.revise_note(&a, &old, "Plan", &format!("v{n}-competing"))
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        let latest = results.into_iter().find_map(Result::ok).unwrap();
        let text = s.read_note(&a, &latest).unwrap();
        assert_eq!(s.revise_note(&a, &old, "Plan", &text).unwrap(), latest);
        assert_eq!(
            s.note_history(&a, &latest).unwrap()["versions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // Restoring content publishes a new revision, never rewinds the old pointer or ACLs.
        let restored = s.revise_note(&a, &latest, "Plan", "v1").unwrap();
        assert_ne!(restored, old);
        assert!(s.revise_note(&a, &old, "Plan", &text).is_err());
        assert_eq!(s.read_note(&a, &latest).unwrap(), text);
    }
    #[test]
    fn external_source_revocation_applies_to_every_revision() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = user(&s, "source@test");
        let b = user(&s, "editor@test");
        let c = user(&s, "reader@test");
        let source = s.save_note(&a, "Source", "RESTRICTED").unwrap();
        grant(&s, &source, &b);
        grant(&s, &source, &c);
        s.read_note(&b, &source).unwrap();
        let first = s.save_note(&b, "Summary", "derived v1").unwrap();
        s.read_note(&b, &first).unwrap();
        let second = s.revise_note(&b, &first, "Summary", "derived v2").unwrap();
        grant(&s, &second, &c);
        assert_eq!(s.read_note(&c, &second).unwrap(), "derived v2");
        s.db.lock()
            .unwrap()
            .execute("DELETE FROM read_grants WHERE resource=?", [source])
            .unwrap();
        assert!(s.read_note(&b, &second).is_err());
        assert!(s.read_note(&c, &second).is_err());
        assert!(s.note_history(&c, &second).is_err());
        assert!(s.revise_note(&b, &second, "Summary", "bypass").is_err());
    }
    #[test]
    fn failed_publication_cannot_publish_git_orphans_or_advance_history() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = user(&s, "owner@test");
        let old = s.save_note(&a, "Plan", "first").unwrap();
        s.db.lock().unwrap().execute_batch("CREATE TEMP TRIGGER fail_revision BEFORE INSERT ON note_revisions WHEN NEW.previous IS NOT NULL BEGIN SELECT RAISE(ABORT,'injected publication failure'); END;").unwrap();
        assert!(s.revise_note(&a, &old, "Plan", "orphan").is_err());
        assert_eq!(
            s.note_history(&a, &old).unwrap()["versions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(
            s.note_history(&a, &old).unwrap()["versions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let new = s.revise_note(&a, &old, "Plan", "published").unwrap();
        assert_eq!(s.read_note(&a, &new).unwrap(), "published");
        assert!(s.search_resources(&a, "orphan").unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn legacy_notes_and_lfs_revisions_survive_restart_and_remain_bounded() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let a = user(&s, "owner@test");
        let old = s.save_note(&a, "Long note", "first").unwrap();
        s.db.lock()
            .unwrap()
            .execute("DELETE FROM note_revisions", [])
            .unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let text = "large".repeat(1000);
        let new = s.revise_note(&a, &old, "Long note", &text).unwrap();
        assert!(s
            .revise_note(&a, &new, "Long note", &"x".repeat(16385))
            .is_err());
        assert!(s.revise_note(&a, &new, " ", "bad").is_err());
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.read_note(&a, &new).unwrap(), text);
        assert_eq!(s.read_note(&a, &old).unwrap(), "first");
        let history = s.note_history(&a, &new).unwrap();
        assert_eq!(history["versions"][0]["id"], new);
        assert_eq!(history["versions"][1]["id"], old);
    }
}
