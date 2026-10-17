# Scoped coordination

The pilot provides generic, pairwise conversations. Rust knows participants, groups, scopes, messages, resources and expiry. Group names and participant text supply meaning; there is no service-provider or customer role in the core. Release v2026.09.27-01 extends the earlier text pilot with shared context and attachments.

## Setup and use

1. Each participant first messages Carbon so their WhatsApp identity maps to a stable UUID.
2. An approval admin creates the needed groups and memberships through the existing reviewed changes.
3. The admin asks Carbon to allow one group to coordinate with another, including shared files if desired. The model proposes `create_coordination_scope {requesters, recipients, resources: true}`. Rust's approval notice describes fresh requests, quoted replies and uploads within each pair. Approve this standing scope once. Omitted/false `resources` creates a text-only scope.
4. A member sends a self-contained request such as “Ask someone in the equipment group when they can help with the projector.” The private planner discovers scoped recipients and calls `start_conversation {scope, recipient}`. No per-conversation or per-message permission question is needed.
5. The recipient uses WhatsApp **Reply** on a Carbon conversation message or delivered file to continue. In a resource-enabled scope, a fresh non-forwarded upload sent this way is automatically available within that pair and staged for delivery to the other participant. Neither person must approve the upload separately. Either can reply **STOP** to close the conversation.

A scope is directional for starts; replies are bidirectional inside the pair. Up to three independent pairs can start from one incoming request. Their histories and uploaded evidence stay separate. An ordinary unquoted message/upload goes to the private assistant; Carbon never guesses the latest conversation. The first event must currently be text; media can continue an established pair using Reply.

Admins discover scopes with `list_coordination_scopes` and propose `set_coordination_scope {scope, enabled}` or `set_coordination_resources {scope, enabled}`. These are reviewed authority changes. Changing either flag changes the scope revision and invalidates existing conversations; it cannot silently upgrade or revive them. Legacy scopes migrate with resources disabled. Group membership or a scope never grants global private-file access.

## Context and file boundary

The private planner cannot pass a draft, private history, file contents, arbitrary address or actor override to the outbound worker. It supplies only a returned scope ID and recipient UUID. Rust copies the exact authenticated current inbound text into the selected pair. Forwarded inputs cannot start or continue a conversation.

The separate scoped model receives only that pair's admitted turns and authorized resource results, labeled A/B. It does not receive private assistant history, unrelated notes, other pairs, phone numbers or identity labels. Rust fixes the destination and appends Reply/STOP instructions. The first request must be self-contained if relevant facts exist only in earlier private history. Model accuracy and anonymization are not deterministic guarantees; participant-supplied shareable messages can contain identifying details.

Resource-enabled runs have bounded `search_resources`, `read_resource`, `send_file`, `analyze_image`, `analyze_document` and `transcribe_audio` tools. They have no shell, arbitrary messaging, permission mutation or private-history tools. There are two resource paths:

- **Existing resources:** both participants must pass the recursive SQLite read/source checks, including inherited grants and dependencies. Search filters access before returning metadata. Knowing an ID never grants access.
- **Fresh quoted uploads:** the exact stored incoming upload, owned by the authenticated sender, receives a pair-local record in `conversation_uploads`. It is not a general read grant: the other participant cannot retrieve it through their private assistant or another pair. Scope/membership/identity revocation, closure and expiry stop further Carbon use. Already delivered WhatsApp copies cannot be retracted.

The fresh upload is automatically staged once, and the model may interpret it when useful. Existing resources are sent only through an authorized `send_file` call. All file types are delivered as WhatsApp documents in this release; photos/audio are not yet native photo/voice-note outbound messages. Maximum attachment size remains 8 MiB. Images use the selected vision-capable model; PDF/scan interpretation and supported audio transcription require the existing xAI-only proxy adapters. No local document/OCR/audio decoder is added. Video interpretation remains unavailable.

Every exposed filename/content is recorded as a task source. Subsequent pair history omits turns whose source access has been revoked and carries forward the dependencies of included drafts. Current runs check their authority epoch and sources after provider calls and before dispatch; a changed epoch conservatively cancels the current output, even if the underlying change was unrelated. This does not retract information already sent to an external provider or participant.

Quoted pair messages cannot change authority. “Approve” in a conversation is ordinary text; admin commands are bound to separate canonical approval notices. On a denied resource, the model is instructed to use available information or ask factual questions, without requesting permission changes.

## Enforcement and durability

SQLite stores scope authority, membership generations, pair contexts, upload grants, source dependencies and durable text/file queues under `/data/state/carbon.sqlite`. Immutable bytes remain Git/LFS-backed under `/data/content`. The complete `/data` backup boundary covers both.

Initial tasks stay staged until the private turn commits. Before model work and dispatch, Rust checks enabled scope revision, both membership generations, exact verified identities, pair state and expiry. Removing then re-adding a member cannot revive an old pair. Unrelated membership changes do not invalidate the pair itself, though any authority change fences an in-progress run.

Each outbound stanza ID is persisted before transport. Continuations require an exact quote from its authenticated destination. Unknown sends are never replayed automatically, but a verified reply quoting their persisted stanza can continue the pair. File rows become pending only after the parent text is confirmed sent; uncertain/failed text cancels its staged files. Access is checked before uploading bytes and again after upload before sending the WhatsApp message.

Restart marks in-flight sends unknown, interrupted uploads/model calls failed, and uncommitted staged work cancelled. Pending known-unsent work may resume only after authorization checks. STOP and unlink cancel unsent work. Failed/uncertain sends appear in grouped activity; automatic retries and operator reconciliation controls remain future work.

Limits: 200 scopes; 3 starts per inbound message; 20 starts per participant and 200 globally per rolling 24h; 24h pair lifetime; 20 inputs per pair; 64 KiB serialized model context; 64 resource sources; 4 scoped model rounds, 4 tools per round and 2 media analyses. Private and pair file delivery share limits of 3 files per incoming request, 20 files/32 MiB per participant/day and 200 files/256 MiB globally/day. Failed/cancelled reservations count. STOP has bounded, model-free reserved admission.

## Verification and current limits

Synthetic tests cover reviewed setup, pair isolation, both-party and derived-source ACLs, fresh-upload provenance, private/forwarded/wrong-recipient rejection, source forgery, STOP, expiry, revocation, identity changes, budgets, upload/send races and restart. Mock HTTP model/provider journeys exercise scoped search, reads, denied private sends, durable authorized file dispatch and provider-only PDF/audio interpretation. No test sends real WhatsApp messages; live media delivery and model quality need separate acceptance.

Instruction bundles, multi-party task orchestration, timers, reminders, richer routing, recovery UI and provider cost budgets remain unfinished. The core remains domain-neutral.
