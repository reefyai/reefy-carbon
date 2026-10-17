# Participant contact consent

Implemented 2026-10-09 for experimental task coordination. Ordinary participants authorize their own contact and task-information sharing; privileged administration remains separately reviewed. No administrator approval or resource-scope upgrade is needed.

## User experience

An explicit request to contact a person, or relevant people selected for the task, can authorize contact directly. Otherwise Carbon sends one combined question: “Contact **Alex** and share information for this request?” The participant uses WhatsApp Reply and sends `approve` or `reject` (case insensitive; `approving`/`rejecting` also supported). Bare approvals do not select a request. Pending requests expire after 24 hours and are not repeatedly created for the same task/recipient.

A grant covers that task's messages and selected relevant attachments. It grants no general access to private notes, folders, history or other recipients' conversations. Replies within an existing inquiry require no reverse grant or repeated approval. Default first contact is allowed between eligible registered participants, within existing quotas.

## Tools and enforcement

- `authorize_task_contact(task, peer, evidence)`: quote exact current authenticated participant text that authorizes contact. It cannot use an internal event, forwarded source or private history as consent. The LLM interprets natural-language intent; Rust verifies source binding and stores an auditable grant.
- `request_task_contact(task, peer)`: stage one canonical combined request. Rust renders the actual proposal and binds its outbound message ID to the owner and verified recipient identities. Only the requesting participant can approve it; even an admin cannot approve someone else's contact consent.
- `open_task_channel(task, scope: "", peer)`: use task-owned consent without an admin scope. Existing resource-enabled legacy scopes remain available; text-only scopes are not silently expanded.
- `send_task_file(task, peer, resource)`: stage an exact selected owner-controlled resource. Reject foreign-owned resources and derived resources with foreign-owned sources. This is delivery authority, not a persistent read grant.
- `revoke_task_contact(task, peer)`: close the channel and revoke its grant. It cannot be revived by another model call.
- `set_contact_preferences(allow_new)`: control receipt of new inquiries; existing channels remain available.
- `block_contact(peer, blocked)`: control the participant's own block list. Blocking closes existing channels in either direction, including legacy channels. Unblocking does not revive them. STOP continues to close incoming task channels.

Consent and actions are staged until the originating turn succeeds; failure or cancellation revokes staged consent and cancels actions. Quoted approval is a Rust control, checked for authenticated sender, exact request binding, delivery state, identity snapshots, expiry, replay, open task and recipient eligibility. Approval wakes the owner task durably. Channels, text/file delivery and schedules recheck current authority. Disabling new inquiries does not retroactively revoke existing consent; blocking or revocation does.

## Persistence and migration

SQLite under `/data/state` stores consents, requests, preferences and blocks. Content remains in Git/LFS under `/data/content`; backup includes all `/data`. Participant channels use nullable `conversations.scope`, with authority supplied by a task consent rather than group membership. Legacy scopes keep their original checks. The migration copies existing records transactionally, checks foreign keys and reenables enforcement before startup completes. File outbox records support task/recipient destinations while preserving ordinary self-delivery.

## Validation and limits

`tests/app/contact_consent_e2e.rs` exercises real Rust intake, storage, HTTP model/tool loop and delivery claims; provider responses and WhatsApp transport are fixtures. Coverage includes no-admin first contact, canonical quoted consent, bare/spoofed/expired controls, restart, replies without reverse approval, owned/foreign-derived files, recipient controls, revoked timers, failed turns and populated legacy migration.

Natural-language consent interpretation and relevance/disclosure judgments remain model decisions, as with existing commitment authorization. Exact source binding prevents fabricated evidence, not semantic misinterpretation. Real-model acceptance and broader adversarial disclosure tests remain necessary. The generic core contains no service categories or business stages.