# Inquiry routing and continuity

Carbon remains domain-neutral: the LLM interprets unquoted messages; Rust binds identities, sources, associations and lifecycle. One participant can receive several inquiries and initiate unrelated private tasks through the same WhatsApp chat.

## Participant history and status

`list_tasks` separates private owned tasks from incoming inquiries, including coordinator UUID, lifecycle and reply availability. `read_inquiry(task, before)` returns only the authenticated participant's exchange with that coordinator: up to 30 messages/32 KiB per page, delivered-file metadata, and separate task/channel/wake states. Use the returned cursor for older history. No owner objective/private notes, other recipients' conversations, private timer purposes or general resource access are exposed.

A completed task may retain participant-readable history while contact authority remains valid, but cannot receive new replies. Revoked/closed/expired channels deny inquiry access. A task-level scheduled wake is not proof of follow-up for a particular recipient. Closing a participant's own helper task does not close another coordinator's inquiry.

## Quoted and ordinary replies

Rust matches a quoted outbound task-message or captured task-file ID to its verified recipient and active channel. Valid replies are associated without running the recipient's LLM; the coordinator receives a durable task event. A stale, wrong-recipient or forwarded quote cannot silently fall back to a different task/private assistant. Approval notices remain on their separate request-bound control path. Ordinary unbound words such as approve/reject go to the conversation model; they cannot apply administrative changes. Recorded admin notices and legacy proposal-ID commands still use Rust approval handling. Historical file IDs that were never captured cannot be reconstructed.

For an unquoted answer, question or update, the LLM reads relevant inquiry history and uses `attach_task_reply` when association is clear. Existing inquiries require no reverse contact consent. Unrelated requests remain private. Never broadcast or select merely the newest inquiry.

## Durable factual clarification

`ask_inquiry_clarification(tasks, question)` holds the current message privately with 2-5 validated candidates and one short question. Rust sends the stored question with a captured message ID when the turn succeeds. This is factual routing, not another permission approval. A held turn can publish only the question; any parallel staged outreach/files are cancelled.

The pending context survives restart. When the participant's current answer identifies a saved candidate, `resolve_inquiry_clarification(task)` associates the saved original message; no repetition is needed. The selection answer is not automatically forwarded. Useful additional facts can be attached separately to the same selected inquiry. Unrelated private requests leave clarification pending; `cancel_inquiry_clarification` abandons it without sharing. Candidates, identity, source and current channel authority are rechecked. Clarifications expire after 24 hours.

## Publication, cancellation and limits

Reply associations are staged until successful turn completion. History tools exclude staged incoming messages. Publication, input completion and coordinator wake commit together; model failure, cancellation or interruption removes staged associations. Restart restores an interrupted selection to pending clarification. If coordinator quota is exhausted, the received message persists and its wake is retried durably under current channel authority. A revocation race cancels publication and frees the participant worker rather than leaving it stuck processing.

Natural-language relevance and clarification interpretation remain model judgments. The core cannot prove whether a clear-looking unquoted reply was semantically assigned correctly. Outbound task-file IDs are captured; forwarding incoming attachment content into a task is a separate capability and is not introduced here.

## Acceptance

`tests/app/inquiry_routing_e2e.rs` covers two requesters/one recipient, scoped and paginated history, helper/task lifecycle distinction, quoted text/files, spoofed/forwarded/revoked quotes, durable clarification, cancellation/restart, coordinator quota and revocation races. The runtime E2E uses the actual Rust HTTP model/tool loop with provider fixtures. An opt-in real-model lane retains complete traces and checks ambiguous selection after restart, ordinary questions, private requests, status, deterministic quotes and cross-participant canaries. WhatsApp transport is simulated.