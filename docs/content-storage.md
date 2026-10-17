# Content storage: Git + LFS, permissions in SQLite

Accepted decision, 2026-09-26. Design only; not implemented in the web starter. This supersedes the earlier standalone revision-file/blob layout.

## Boundaries

- SQLite is authoritative for participant UUIDs/bindings, groups, ownership, logical folders/display names, grants, version visibility, published content references, 2PR, messages, jobs, outbox and audit.
- An internal Git repository at /data/content versions participant uploads and Carbon-authored resources: Markdown notes, profiles, live instructions, documents and media. Git commits are immutable content revisions; no explicit v1/v2 directories.
- Git LFS holds large-file bytes locally under /data/content/.git/lfs/objects. Git stores their pointers. Size/type routing and limits must be deterministic and tested; choose the threshold during implementation.
- Config, credentials, WhatsApp protocol state, SQLite files, staging, derived indexes and logs belong under /data/state, outside the repository. Raw conversation records remain in SQLite; saved summaries are Git resources.
- No authoritative ACL sidecars. Git authors, filenames, content hashes, repository membership or commit history never grant participant access.
- No remote or Git hosting service is required. Do not push private runtime content into the application source repository.

## Layout

Illustrative IDs below stand for UUIDs:

```text
/data/
  state/
    config.json
    secrets/
    carbon.sqlite                     # permissions and operational state; include WAL/SHM
    whatsapp/
    staging/
    derived/
    logs/
  content/
    .git/                         # Git history and retention refs
      lfs/objects/                # local large-file bytes, required persistent data
    .gitattributes                # Carbon-managed LFS rules
    resources/
      <resource-uuid>/content.md
      <resource-uuid>/content.jpg  # pointer in Git; full file when materialized
```

This is the target coordinator layout. The web starter still uses /data/config.json and /data/secrets/admin-password; adopting /data/state in runtime packaging requires an explicit migration preserving existing config/credentials. Do not silently initialize a new identity/password when old state exists. Architecture examples use this target layout; deployment commands describe the current starter until that migration lands.

## Local Git LFS operation

No Git server, LFS server or always-running LFS daemon is needed for this single-device repository. The standard LFS clean operation writes actual bytes into the local .git/lfs/objects store and returns a small pointer containing the format identifier, SHA-256 object ID and size for Git to commit. A materialized checkout can contain a second full copy of the current file; retained previous versions stay in the LFS object store.

An authorized Carbon read resolves the SQLite-published commit/path and backend-recorded storage kind. For an LFS version, validate the pointer against the trusted ingest record's payload SHA-256 and byte size, then read/verify the matching local object. Do not infer storage kind or ownership merely because uploaded bytes resemble a pointer. It must not attempt an automatic remote fetch or treat a missing object as usable content. Network transfer would require an explicitly configured LFS endpoint or transfer mechanism only if remote synchronization is introduced later.

The entire local LFS store is required application data, not disposable cache in our deployment. Back up all of /data; ordinary Git push/clone alone does not transfer that complete state. LFS is an extension/integration, not functionality automatically supplied by every Rust Git library. Choose and test the backend without giving the model process access.

Reference: [Git LFS specification: pointers, local object storage and clean/smudge](https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md).


Stable internal paths identify resources; names and logical folder moves are SQLite metadata. A SQLite version row references an exact commit, internal path, content checksum and, when relevant, LFS object ID/size. Reads never use a mutable working tree or assume branch HEAD equals published application state.

## Uploaded bytes are not trusted storage references

Hash and measure the actual admitted payload stream independently of Git filters. SQLite records backend-selected storage kind, payload checksum/size and the exact Git reference. Only Carbon generates an LFS reference from that admitted payload. A supplied object hash or an existing local object is never evidence that the uploader owns or may reference those bytes.

This matters because the standard [Git LFS clean implementation](https://github.com/git-lfs/git-lfs/blob/main/commands/command_clean.go) preserves an already-formed pointer instead of wrapping its text as a new payload. Blindly accepting that result could register a pointer to another participant's private media as the attacker's resource. Handle pointer-looking uploads as literal payload using a tested storage path, or explicitly reject them; never implicitly dereference them. Any future resource-copy operation authorizes the source/version and destination separately.

Test a syntactically valid forged pointer to an existing private LFS object. The attacker must receive only their literal submitted bytes or a rejection, with no private read, index, thumbnail or model input. Unknown hashes and recursive pointer payloads must not create a reference lookup oracle.

## Coordinated writes and recovery

Git and SQLite cannot share an ordinary transaction. Preserve a content-first publication protocol:

1. Create a durable operation ID/intent and quota reservation in SQLite, after validating actor, input and expected resource version. This does not publish content or grant access.
2. Under a serialized content writer, persist validated bytes, required LFS objects, Git objects/commit and a retention ref. Verify actual LFS availability/checksum and configure/test durable flushing before referencing the result in SQLite. No database transaction is held across content preparation.
3. In a short SQLite transaction, recheck current authority, expected resource/policy versions and any exact 2PR approval. Publish the commit/path reference, applicable grants/version visibility, settled logical byte attribution, audit, operation completion and outbox together.
4. Acknowledge success only after that transaction commits. Permission-only changes stay entirely transactional in SQLite.

A crash before publication may leave a retained Git commit, but no participant gains access to it. A prepared operation is not proof of permission: recovery must revalidate or cancel it. A completed operation is not applied twice. Git HEAD alone must never auto-register resources or rebuild grants.

Use explicit non-expiring refs for prepared/published content required by SQLite; reflogs and a mutable HEAD are not the retention contract. Test actual Git object/ref and LFS durability on the selected backend. Any maintenance that rewrites/deletes objects must coordinate with active preparation and reads.

Garbage collection/pruning must preserve all SQLite-referenced versions, pending ingestion, retention requirements and relevant Git refs/LFS objects. Do not run default pruning without proving those invariants. Missing objects cause unavailability, not fallback to another version or broader access. Corruption cannot be repaired by inventing ACLs.

### Deletion and retained space

Initial deletion is a tombstone, not erasure or reclaimed quota. A retained shared Git commit can keep trees and ancestors containing other participants' historical bytes. Deleting a path from the current tree does not purge that history. Selective erasure may later require rewriting history and transactionally remapping retained SQLite commit references, plus LFS, derivative and backup handling. Keep this deferred only while no selective-erasure deadline is promised; if that becomes a near-term requirement, settle commit topology and purge design before rollout. See [Git garbage collection](https://git-scm.com/docs/git-gc).

## Access and history

Carbon alone owns repository access. Rust tools accept resource IDs, not arbitrary Git commands, paths, revisions or remotes. Authorize requested historical versions using current SQLite policy and version visibility. Do not expose repository clone, raw Git/LFS HTTP endpoints, unrestricted diff/log output or the shared checkout to participants or the LLM. A single commit may include other participants' data.

Restore content by publishing a new authorized version. It never rolls back grants, group membership, approvals, jobs or external effects. Shared current content does not imply access to its private predecessors. Search/derived content keeps source resource/version references and current authorization checks.

## Trusted Git integration

Use a vetted Rust integration or fixed backend-controlled invocations; the LLM gets no shell or process tool. Never execute uploaded hooks, arbitrary filters, external diff/textconv drivers, submodules or remote URLs. Only Carbon-managed LFS behavior is allowed. Reserve .git, .gitattributes and related control paths; imports are resource bytes, not trusted repositories/configuration. Disable ambient Git configuration and automatic network fetching in content reads. Choose and verify the actual Git/LFS implementation before deployment.

## Backup and tests

Back up all /data, including the complete Git directory, retained LFS objects, SQLite WAL/sidecars, staging and operational secrets. A Git clone/push is not a complete Carbon backup. Manual raw copies require stopped Carbon; Reefy snapshot recovery must be tested. Validate database integrity and every retained published Git/LFS reference before resuming work.

The [testing contract](testing.md) applies from the first storage feature: real local Git/LFS fixtures, file-to-pointer routing, restart/restore, crash boundaries, stale expected versions, missing LFS objects, retention/GC, denied raw-history access and cross-participant model/output leakage. These tests must not substitute mocked storage or authorization for real implementation.
