# Versioned notes (v2026.09.26-19)

Participants can ask Carbon to update a saved note, list its previous versions or restore earlier content. Carbon uses `read_note`, `revise_note(base, name, text)` and `note_history(id)`. Editing is owner-only; read access, group administration and approval-admin status do not confer editing rights.

Example private conversation:

- “Remember that we need one filter.”
- “Update that note: we need two filters.”
- “Show me the previous version.”

Ordinary private edits and history reads require no approval. Each edit saves a new immutable resource with a new exact ID, linked to its original note by SQLite `note_revisions`. The base ID is the concurrency token: a competing or stale update is rejected before writing bytes. Retrying an identical immediate update returns the already-created revision; it neither duplicates storage nor changes permissions. To restore earlier text, read that version and save it against the latest base. Never roll back ACLs, Git HEAD or operational state.

## Visibility and publication

A new revision starts in the owner's private root. It does not copy read grants or logical folder placement, including shared-folder inheritance. Old content, its grants and folder remain unchanged. Explicit sharing of the new exact resource uses the existing reviewed grant/move flow. This first slice does not provide delegated editing, automatic shared updates or a mutable “current shared version” alias.

History lists only revisions the caller can read now, newest first. It reveals neither hidden IDs/titles nor total revision numbers. Read permission on a later version does not expose private predecessors. Listing and keyword search return the newest currently readable revision for each note; readers of an older shared revision continue to see it when a newer revision is private. Use history for earlier content. Search retains its existing bounded index/candidate limits.

An edit retains all external source restrictions from its previous revisions and current context. Same-owner predecessor links are version metadata, not mandatory disclosure grants: publishing a revision need not publish old private text. Dependencies outside that lineage are conservatively preserved, including transitive dependencies; removing words from an edit does not erase provenance. Revoking a source can therefore deny reads, history and further edits until access is restored. Notes are still untrusted reference content, never trusted executable instructions.

## Storage and recovery

SQLite publishes the new resource, revision linkage, provenance and audit entry in one transaction after Git/LFS objects and retention references are durable. The existing serialized content writer is reused. A failed transaction can leave retained orphan bytes, but no readable resource or advanced version history; restart must not infer publication from Git HEAD. All state remains under `/data/state` and `/data/content` and is included in the existing whole-data backup.

Existing notes migrate as one-version histories with unchanged IDs, ownership and grants. Uploaded files and folders are not editable through this tool. Notes retain the 16 KiB text / 120-byte name limits. Each saved revision counts toward the current 100-resource-per-owner and storage limits; no automatic pruning or compaction is added. Downgrading to an image without revision handling would lose current-version presentation and edit concurrency guarantees; use a consistent reviewed restore rather than running an older image on active revision data.

## Verification

Tests cover competing edits, stale bases, retry idempotency, immutable originals, restoration, owner-only writes, hidden predecessor/successor metadata, shared-folder isolation, publishing without predecessor disclosure, external-source revocation, legacy migration, LFS/restart integrity and injected SQLite publication failure after Git writes. A mock-model E2E journey reads a note, revises it, retrieves history and delivers the reply only to the requesting participant; forged actor/path arguments and other users' notes are rejected.
