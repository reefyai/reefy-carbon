# Participant identity and permission consistency

Accepted architecture, settled 2026-09-26. Not yet implemented.

## Accepted storage architecture

- SQLite is the single authority for resource identity, logical names/folders, ownership, permissions and revision references.
- Resource IDs are stable application UUIDs. Physical content uses opaque stable internal filenames; a display-name change updates database metadata rather than renaming bytes.
- Content revisions are immutable. New content is durably written first, then its reference is committed transactionally with the relevant metadata.
- Content lives in the internal /data/content Git repository, with Git LFS for large files. SQLite publishes exact commit/path references and keeps all ACLs authoritative. See [content storage](content-storage.md) and [backup policy](backup.md). Git revisions replace explicit version directories; no ACL sidecars are introduced.
- Do not use inode numbers as permanent IDs. No extended-attribute tracking or filesystem watchers initially.
- No live permission sidecars or duplicated ACL authority. Future human-readable exports may include permission manifests, but they cannot silently restore or override live grants.
- Use filesystem durability and safe path-resolution primitives in the storage implementation. These support SQLite/Rust enforcement without becoming a second identity or permission system.

## Stable identity, replaceable channel bindings

Use a randomly generated UUIDv4 for each participant. UUIDv7 is also valid, but Carbon does not need time-sortable identity IDs; UUIDv4 avoids embedding a creation timestamp. UUIDs identify records, not authorization. See [RFC 9562](https://www.rfc-editor.org/rfc/rfc9562.html).

On the first accepted live inbound WhatsApp message:
1. Validate the transport event and resolve the actual sender, distinct from the conversation/group ID. Reject echoes, history imports and forwarded authors as enrollment/approval evidence.
2. Look up its scoped provider identity binding. The scope includes installation and channel/account namespace. Normalize phone numbers only when supplied through a trusted provider mapping; do not assume LIDs/JIDs can be parsed into phone numbers.
3. If no active binding exists, create a participant UUID plus a binding and source-message record in one transaction. Enforce uniqueness for active scoped identities and deduplicate provider events; concurrent first messages reuse one participant.
4. Give the participant only initial private access. Admission/invitation policy still applies; an unknown sender can be pending or rejected and must not automatically join shared groups or become an admin.

Ownership, membership, grants, messages and jobs refer to the permanent participant ID. Names and phone numbers are attributes/bindings. A message verifies control of a channel endpoint to the transport's assurance level, not a real-world identity or community membership. Never merge participants because the LLM recognizes a name or because two messages claim to be the same person.

For a number change, verify the new endpoint and authorize the transfer using the old trusted identity or an established recovery process. Require 2PR for a binding transfer/recovery under the protected policy, with the requester distinct from the reviewer. An admin must not simply accept “this is my new number.” Preserve the participant UUID, retire the old binding, invalidate affected sessions/pending deliveries, and record the change atomically. Re-resolve recipients before dispatch. Do not inherit a provider messaging window.

If a new endpoint already has a provisional participant, use a reviewed reconciliation operation; do not silently merge histories or grants. Phone numbers can be recycled: no design can equate possession forever with the original human. Provide explicit binding suspension/reverification/recovery and protect privileged operations accordingly.

## One authoritative permission store

Keep the live permission graph in /data/state/carbon.sqlite. Suggested conceptual tables, not a migration/schema contract:

| Records | Purpose |
|---|---|
| principals | Common IDs for participants/groups, allowing real foreign keys |
| participants / channel_bindings | Stable identity and verified endpoint mappings |
| groups / memberships | Group membership and current scoped capabilities |
| resources | Owner principal, logical parent folder, current revision, lifecycle state |
| resource_versions / blobs | Immutable content references, checksums, version visibility |
| grants | Resource/folder, principal, allowed operation and inheritance scope |
| approvals / audit / outbox | 2PR decision, attribution and durable notification work |

A folder is a resource with child relationships. Example: resource R is owned by participant P; group G has read on folder F; R is a child of F. Carbon computes effective access from that graph. A blob path never grants access. Publishing a current revision does not automatically publish private historical versions.

Use one ACL authority; do not mirror live permissions in Markdown frontmatter, sidecar JSON, filesystem chmod entries and SQLite. Filesystem ownership protects the volume from other OS users; all Carbon participants still access through the app's single OS identity. Rust enforces per-participant policy; SQLite does not supply built-in row-level authorization.

Permission metadata is part of the actual data, not a disposable index. Losing the database loses ownership/grant information even when file bytes survive. Restore database and files from the same recovery point. Optional human-readable exports may aid inspection/recovery but must never silently override current policy or auto-grant access.

## Generic coordination scope and intake

A protected coordination scope specifies standing authority using principals/groups, allowed recipient sets, resource/source bounds, reply relationships, capabilities, limits and expiry. It has no business role or process semantics. Human meanings belong to LLM instructions and content.

Verified message/reply correlation can select an existing context mechanically. Otherwise the LLM proposes creating/selecting a context from scopes already invocable by the actor. Rust validates scope, current rights, actual source IDs, provenance and recipient bounds; it does not decide whether a message means “repair” or any other domain intent.

Configured intake rules attach permitted use to incoming events. Selecting a context cannot widen those original bounds or relabel historical/private resource IDs as fresh input. Creating the scope's bounded grants/delegation is routine execution with no permission ask or 2PR; changing standing authority remains protected administration.

Allowed recipient sets are selection bounds, not blanket read access. Record exact resource/reply audiences for each context and isolate overlapping conversations. The [Aventura Bay example](aventura-bay-example.md) maps these generic relationships to customers/providers; those labels have no core meaning.

Private history, identity fields and private planning outputs retain their scope. Gather current evidence or omit unnecessary inaccessible material without permission chatter. Interpreting private intent or ambiguous prose is model behavior; explicit structured privacy/context flags and access checks are deterministic. No authorization mechanism proves arbitrary text/media anonymous.

## Information flow and publication

Checks on file reads and message sends are insufficient by themselves. A run that has read private material must not write it into a shared note, resource title, instruction, job description, cache or query and thereby make it readable elsewhere. This applies even when its execution identity can edit that destination.

Record the resource/message/instruction versions and audience restrictions of all context actually supplied to the model. Model citations cannot narrow that set. For ordinary generated output, readers must satisfy the destination's grants and every unreleased source restriction. If additive folder inheritance would make those conditions impossible, place the draft outside that shared folder or regenerate in a context readable by the destination audience. Validate at write/publication, retrieval and later dispatch, including derived indexes and history. Participant-supplied metadata cannot forge provenance.

Explicit publication is a deliberate release of exact output bytes to an identified audience. Require publication authority over each restricted source being released; ownership of a new summary is not authority over its inputs. For example, Alice can read Bob's private document and own a generated summary without being allowed to publish Bob's information. If authority is missing, first use already permitted sources or omit unnecessary material. Obtain the source controller's scoped approval only when that new disclosure is necessary to fulfill the request. A standing grant making selected source material readable by the target audience already authorizes ordinary derived drafts within that scope; do not ask for each wording revision. No LLM redaction/classification can authorize release.

A release record binds the output version, audience and authorizing identities to the selected source versions. Routine derivatives remain source-restricted; an explicitly released resource has its own authorized grants only within that release's scope. Keep the release ceiling and provenance on internal copies: widening grants, moving into broader inherited access or copying to a new audience must again satisfy the applicable source-controller release authority. Owning the released output does not make an audience-limited release unrestricted. Do not silently undo a valid release when a source is later edited, or silently widen it when a group changes. The precise grant/history lifecycle is specified before schema freeze.

Core prompt text may be a non-secret application policy supplied to all runs. Domain instructions are ordinary permissioned inputs as well as trusted steering: instruction-edit authority does not make confidential instruction text safe to place in an outsider-facing prompt. Never put infrastructure secrets into model context. Model-provider processing, web search and participant forwarding have distinct destination policies.

### History and grant lifecycle

Represent visibility using immutable version identity and a grant/publication lifecycle, not only a resource/group pair. Counterexample: share v1 with G; revoke G; keep v2 private; regrant G at v3. Combining the new current grant with an old G/v1 publication would resurrect history that the new grant did not include. A grant epoch or equivalent explicit version-range mapping must prevent this; inherited grants, moves and ownership transfer need the same treatment.

New group membership versus a new resource grant are separate events. Whether joining G includes G's previously published history is still a product choice. Default recommendations are recorded in the architecture review, not encoded by an accidental SQL join. Revocation blocks future Carbon reads; it cannot erase copies already delivered.

### Message audiences and administrative review

A correlated reply follows configured participant/conversation relationships and the context's recorded audience. No per-context or per-message forwarding confirmation is required within that authority. Without a covering scope keep it private and continue with permitted participants/material. Rust checks sender/context/audience IDs, not whether the reply is a quote or another business fact.

2PR review authority and disclosure authority are independent. Canonical approval details include the exact operation and permitted metadata, not unrestricted private contents or participant directory data. A reviewer unable to inspect the necessary scope cannot supply informed approval; choose an eligible reviewer with appropriate visibility or obtain a scoped disclosure first.

## Transactional permission changes

Use short transactions for authorizing and applying a change. Recheck current actor/reviewer authority and expected resource/policy revisions inside the transaction; apply membership/grants, consume the exact 2PR approval, append the audit event and insert any outbox record together. An operation either commits completely or rolls back. Do not hold transactions open across model/network calls.

For SQLite: enable and verify foreign_keys on every connection, use NOT NULL/UNIQUE/CHECK constraints and foreign keys for identities/owners/grants, and prevent destructive deletion of referenced principals. Validate hierarchy cycles and inheritance changes in Rust. WAL with synchronous=FULL is the proposed durability baseline on device-local storage; benchmark it without silently weakening persistence. SQLite's durability depends on functioning filesystem/storage synchronization.

The transactional outbox keeps notification intent consistent with the database. Delivery happens later with retries/idempotency and fresh access checks; it does not make external messages exactly-once or recall already sent data.

Sources: [SQLite atomic commits](https://www.sqlite.org/atomiccommit.html), [foreign keys](https://www.sqlite.org/foreignkeys.html), [WAL](https://www.sqlite.org/wal.html), [synchronous settings](https://www.sqlite.org/pragma.html#pragma_synchronous), [AWS transactional outbox](https://docs.aws.amazon.com/prescriptive-guidance/latest/cloud-design-patterns/transactional-outbox.html).

## Git/LFS and SQLite publication

Follow the [content-first publication and recovery protocol](content-storage.md#coordinated-writes-and-recovery): persist Git/LFS content and retention refs, then transactionally recheck authority and publish the exact reference with metadata/permissions/audit in SQLite. Git HEAD is not application publication. Prepared commits grant no access; recovery never infers permissions from Git.

All history, search, derivatives and LFS reads remain resource/version-authorized. Garbage collection must preserve published and pending references. Git/LFS and SQLite do not share an ordinary transaction; crash and restore tests are mandatory.

## Failure, revocation and recovery

- Missing permission metadata, failed authorization queries, DB corruption or an unavailable DB mean deny/stop; never fall back to “owner by folder name,” public access or a fresh empty database.
- Avoid authorization caching initially. Later caches require explicit invalidation/version checks; queued work must reauthorize at execution. Define dispatch ordering with revocation: serialize the final authorization/dispatch decision against policy changes. Once an external send has begun, revocation cannot guarantee recall.
- Run database integrity and separate foreign-key checks during restore/migration and appropriate maintenance. Reconcile references/checksums and quarantine anomalies. Never infer missing ACLs from surviving files.
- SQLite backup APIs give a consistent database snapshot, not a consistent snapshot of separately changing blobs by themselves. Use the complete Reefy volume snapshot or a coordinated/stopped export as described in [backup policy](backup.md).
- Restore can roll permissions back in time. A known restored instance quarantines all normal participant access, including reads/search/model context as well as sends/jobs, until trusted recovery reconciles recovered authority and external effects. A valid old database cannot reveal revocations made after its snapshot; see the platform restore/takeover requirement in [backup policy](backup.md#restore-contract).
- Audit records improve diagnosis but are not tamper-proof against a host administrator. A valid-but-wrong grant caused by an application bug can pass every integrity check. Test the authorization matrix, cross-participant searches, grant/revoke races, relinking, crash points and old-backup restores.

This follows [OWASP's authorization guidance](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html): deny by default and validate authorization on every request. SQLite plus private file storage is our architectural recommendation for Carbon, not a claim that transactions eliminate every possible failure.

## Established systems and lessons for Carbon

Research updated 2026-09-26. Separating document bytes from relational ownership/permission metadata is an established application architecture. It does not require a distributed authorization service for this single-device app.

| System | Evidence | Lesson |
|---|---|---|
| Paperless-ngx | Documents reside in its media volume; the database is separate. Its API exposes per-object owner, user/group view and change permissions. | A close practical analogue for Carbon's files plus application-managed access. |
| Nextcloud | Keeps sharing metadata in its database alongside separately stored files. Its external-storage documentation explains how direct renames can lose shares and other metadata. | Stable resource IDs and application-mediated edits matter; scanning files cannot reconstruct authority. |
| Seafile | Stores library owners in a relational database, with versioned content represented as objects/blocks outside it. | Related architecture, not an example of ordinary editable Markdown folders. Its recovery procedure explicitly checks cross-store consistency. |

Sources: [Paperless administration](https://docs.paperless-ngx.com/administration/), [Paperless permissions API](https://docs.paperless-ngx.com/api/#permissions), [Nextcloud external-storage changes](https://docs.nextcloud.com/server/stable/admin_manual/configuration_files/external_storage_configuration_gui.html#detecting-changes-made-outside-nextcloud), [Seafile data model](https://manual.seafile.com/11.0/develop/data_model/).

Nextcloud's [backup procedure](https://docs.nextcloud.com/server/stable/admin_manual/maintenance/backup.html) retains both data and database and uses maintenance mode to avoid inconsistencies. Seafile's [backup/recovery guide](https://manual.seafile.com/12.0/administration/backup_recovery/) explicitly discusses backup ordering and checking references after restore. These are evidence that cross-store consistency must be designed, not evidence that it comes automatically. Do not transplant a backup ordering recipe without its retention/concurrency assumptions.

### Concrete Carbon rules

- **Only Carbon mutates live managed content.** Markdown, images and videos remain ordinary formats, but live folders are not an independently editable shared drive. WhatsApp/web/typed tools all pass through one resource service. A future bulk import or editing workflow must explicitly register changes with Carbon; no watcher may invent ownership or grants.
- **Authorize resources, not paths.** A stable resource UUID identifies a document; name, parent and content version can change. Rust resolves an authorized resource to an internal path. Reject path traversal and symlink escapes. A UUID or checksum is never permission.
- **Treat content revisions as immutable.** Write a new revision, then transactionally update the resource pointer. Avoid overwriting the bytes under existing metadata. This preserves the files-first durability rule above and makes checksum reconciliation meaningful.
- **Make folder moves permission-aware.** Moving a resource can change inherited access. Re-evaluate the destination, require authority for the resulting access change, and apply protected changes through 2PR. Renaming within a folder need not change ownership. Physical storage paths are not the policy graph.
- **Restrict every derived representation.** Thumbnails, OCR, excerpts, search results, summaries, old revisions and LLM context need source-aware authorization. Checking only original-file downloads is insufficient.
- **Keep repair conservative.** Add an internal diagnostic operation that reports missing blobs, checksum mismatches, orphan files, invalid references and hierarchy problems. Quarantine anomalies; do not automatically attach unknown bytes to a participant or publish them. Garbage collection must coordinate with ingestion.
- **Back up and restore the whole state.** Keep the selected single /data volume. For a simple manual backup, stop Carbon and copy all of it; for Reefy snapshots, verify the actual recovery behavior including SQLite WAL files. Test that restored private resources remain private and revoked access does not silently resume.
- **Test interruption boundaries.** Crash between file write and DB commit; repeat uploads; race revocation with queued work; restore an older snapshot. Required result: failure can make content unavailable, never grant additional access.

These are Carbon design choices drawn from the documented operating constraints, not claims that all three products implement Carbon's exact protocol. Existing database constraints/transactions and mandatory Rust checks remain the authority; no duplicated live ACL in sidecar files.

## Scope decision

SELinux-specific integration is out of scope for Carbon. No SELinux policy, label synchronization or dependency is planned. This does not require changing or disabling any host security configuration.

## Deferred: application-level encryption

Decision: defer application-level content encryption and per-participant/per-resource key management. The accepted SQLite authority, stable internal filenames and immutable revision architecture remains unchanged. Do not introduce encryption keys or encrypted content formats in the initial implementation.

Before revisiting, assess Reefy's existing disk and backup protection, define the threats encryption should address, and settle root-key protection, device replacement/recovery, rotation and coverage of SQLite/derived data. This deferral does not disable or change platform-provided encryption and makes no claim that existing Carbon storage/backups are encrypted. All Carbon-owned persistent state remains within the current /data backup boundary.
