# Implementation status

Reviewed against the running Rust code on 2026-09-27, release v2026.09.27-02.

## TODO: WhatsApp messages queued during downtime

- [ ] Verify the pinned WhatsApp library's queued-delivery versus historical-sync event semantics. Do not treat every offline flag as historical content.
- [ ] Admit legitimate messages queued while Carbon was disconnected through normal authenticated intake, permissions, limits and durable inbox processing after reconnect.
- [ ] Deduplicate by stable provider message identity across reconnects and process restarts; previously handled messages must not generate another model run, reply or action.
- [ ] Keep historical chat synchronization separate from actionable incoming messages; establish and document the boundary before changing admission behavior.
- [ ] Improve admission diagnostics with safe sender/message correlation and a precise reason, without logging message content unnecessarily.
- [ ] Add a first-class restart/reconnect E2E through the real Carbon intake, persistence and worker path with a fake WhatsApp adapter. Cover a message arriving during downtime, queued delivery on reconnect, repeated delivery after another restart, and historical-sync events. Assert the queued message is persisted and processed once, duplicate deliveries cause no repeated actions/replies, and history synchronization does not start unsolicited work. Retain negative admission/permission coverage.

No runtime or deployment change is authorized as part of recording this TODO.

## Working now

- Native typing presence during active private/pair processing, refreshed every 10 seconds, with one fixed progress notice after 25 seconds. Progress notices are deduplicated durably and pair replies remain correctly routed. Quoted STOP bypasses the model queue to cancel a pair immediately. See [processing feedback](processing-feedback.md).

- Native primary/linked-device message normalization from PR #1 is included in the default release.
- Standing group-to-group scopes, isolated pair contexts, exact quoted-reply routing, STOP, expiry, revocation and durable delivery states; see [scoped coordination](scoped-coordination.md).

- Reviewed resource-enabled scopes allow mutually readable notes/files, pair-local fresh quoted uploads, provider image/PDF/audio interpretation and durable cross-participant file delivery. No per-upload approval is needed. Existing text-only scopes remain text-only until explicitly changed.
- Native WhatsApp session persistence, QR and phone-code linking, unlink and on-demand QR renewal.
- Collapsed setup sections, Reefy login integration/optional app password, model selection, initial reviewer sealing and bounded persistent activity logs grouped by durable message operation IDs.
- OpenAI-compatible Reefy LLM proxy calls with bounded rounds and bounded tools for notes, directory lookup and permission proposals.
- Participant UUIDs mapped to incoming WhatsApp identities, private message history and note ownership checks.
- Immutable notes, uploaded files and logical folders; read grants to participants, groups or everyone, inherited folder grants and reviewed moves. Group administration and memberships use 2PR.
- Durable, bounded WhatsApp photo/document/audio/video ingestion into Git/LFS; private ownership, upload deduplication, integrity validation and pending-download restart recovery. Small UTF-8 text files can be read; JPEG/PNG/WebP analysis is available with a vision-capable model. Files and notes can be delivered back to the current participant through a bounded durable queue. Provider-based PDF interpretation (including scans) and audio transcription are implemented for xAI-only proxies; video understanding remains planned.
- Concise reply-bound WhatsApp approval notices and a collapsed read-only Admin approval requests web panel.
- Durable admin approval proposals, canonical WhatsApp notices, exact case-insensitive request-bound commands, 24-hour expiry, one or more configured admins; self-requested changes may be explicitly approved, transactional revalidation, audit and replay protection.
- Natural-language approval-admin additions/removals with explicit human review, per-person role generations, last-admin protection and pending-request handoff.
- Owner-only note revisions, readable version history, stale-write protection and private-by-default edits; see [versioned notes](note-revisions.md).
- Keyword search over authorized resource names, notes and bounded UTF-8 files using a rebuildable SQLite FTS5 cache; pending-review discovery with actor-bound visibility.
- Conservative note provenance, recursive source-access checks and a context/output barrier after access changes.
- QR preparation spinner; sections remain collapsed and pairing stays on demand.
- SQLite, durable inbox/outbox, replay deduplication, daily accepted-request limits, immutable Git/LFS-backed text notes, and whole-data backup enrollment.
- Unit/integration tests, mock-model tool E2E, Linux container smoke tests and opt-in live QR lifecycle tests.

## Immediate next work

Prioritize task-event freshness, private-limit/recipient disclosure controls, appointment-aware follow-up E2Es, then reproducible release/restore verification. Exact acceptance and evidence are in [the comparison follow-up plan](model-comparison-2026-09-27.md#next-implementation-order). The broader backlog below remains; it is not a claim that existing task/scheduler primitives must be rebuilt.

## Broader remaining work

1. **Extend resource permissions.** Delegated folder editing, automatic shared-version publication, scoped rather than global private-context invalidation. Immutable resources, logical folder hierarchy/inheritance, same-owner reviewed moves and source-based revocation checks are implemented. Keep the core domain-neutral.
2. **Administration recovery.** Protected recovery when no admin account is available and reconciliation of uncertain notification delivery remain. Reviewed additions/removals of approval admins, last-admin protection and pending-request handoff are implemented; see [admin management](admin-management.md).
3. **Media and live knowledge.** Additional provider-supported document/audio formats, shared-version publication, live bundles edited through messages, and semantic search. Keyword search and pending-review discovery are implemented; see [search and reviews](search-and-reviews.md). Upload storage, provider PDF/scan interpretation and Ogg/WAV/MP3/FLAC transcription are implemented; other formats and video interpretation are not.
4. **Multi-party coordination.** Pairwise scopes, recipient discovery, isolated prompts, authorized shared context, cross-recipient files and exact reply routing are implemented. Experimental task orchestration, normal-reply association and timers are now implemented. Remaining: ambiguous/concurrent-task routing, recipient-safe draft provenance, richer adversarial coverage and live instruction bundles. Keep routine coordination free of per-message permission prompts within standing authority.
5. **Scheduling and recovery.** App-owned durable one-shot timers, follow-ups and execution-time authority checks are implemented. Remaining: appointment/timezone-aware follow-up acceptance, operator reconciliation for interrupted model work and uncertain sends, and broader recovery/lease coverage. Never replay an uncertain send blindly.
6. **Identity continuity and operational controls.** Verified alias/phone relinking with UUID continuity, adjustable admission controls, model/token/cost and outbound budgets, friendly limit responses, per-user concurrency, data retention/compaction and resource quotas.
7. **Production verification.** Cross-host/restore activation fencing, backup/restore drills covering SQLite and Git/LFS, transport reliability on the current x86-64 target, and a broader deterministic multi-party E2E suite for ACLs, revoked access, 2PR, prompt injection, retries and concurrent changes. Validate two unrelated coordination bundles to ensure the core stays generic.
8. **Later integrations.** Controlled web search/browsing and other capability adapters; optional official WhatsApp Cloud API/webhook transport. Per-resource encryption remains explicitly deferred.

Owner-only note editing/history is implemented. Delegated editing and shared-update publication remain unfinished. Attachment storage, logical folders and inherited read access are implemented and tested. Image understanding is implemented. File replies to the current participant are implemented. Provider PDF/scan interpretation and speech transcription are implemented. Recipient-scoped coordination includes shared-resource context and cross-recipient attachments. ARM64 is deferred; development and deployment currently target Linux x86-64. Routine coordination must continue without per-message approval prompts inside established authority. See [attachments and folders](attachments-folders.md) and [the permission pilot contract](permissions-pilot.md) for current constraints.
