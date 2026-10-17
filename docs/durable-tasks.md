# Durable coordination tasks

The Rust task layer is domain-neutral. It knows owners, participants, scopes, messages, revisions, queues and schedules. It does not know service categories, symptoms, prices or a prescribed conversation sequence. The LLM decides which task to create, which resources to inspect, whom to contact and what to say.

## Availability

`"experimental_tasks": true` in `/data/state/config.json` enables durable tasks. It defaults to false. Acceptance fixtures enable it only in disposable data directories. Existing instances retain their explicit settings.

This is an experimental coordination implementation, not a claim of production security or reliability. Keep the feature opt-in while live acceptance and adversarial privacy/authorization coverage develop. See [the acceptance test](ac-journey-e2e.md).

## Persistent records and tools

SQLite in `/data/state/carbon.sqlite` stores tasks, immutable note revisions, channels, message/action records, routing associations, task events and one-shot wakes. Existing resources stay in Git/LFS under `/data/content`. Backup still includes all `/data`.

- `create_task`, `list_tasks`, `read_task`, `update_task`: authenticated-owner tasks with optimistic revision checks. Completing or cancelling a task cancels pending sends and schedules. Participant access to a channel does not grant access to its owner's objective, notes or other channels.
- `read_messages`: cursor-based retrieval of the current participant's own incoming messages, with internal task events distinguished from participant messages. Task mode does not preload unrelated private chat history.
- `open_task_channel`: accepts participant-owned task consent with an empty scope, or a reviewed resource-enabled legacy scope. Ordinary contact needs no administrator approval or separate resource-sharing upgrade. Legacy scopes retain their identity/membership-generation checks. See [participant contact consent](participant-contact-consent.md). Opening a channel sends nothing. Revoked channels cannot be silently revived.
- `send_task_message`: stages bounded text for one verified channel. Successful completion of the model turn publishes it to the durable outbox. Failed turns cancel staged actions. Delivery rechecks authority; uncertain sends are not automatically replayed after restart.
- `record_task_authorization`, `send_task_commitment`: bind immutable terms and a recipient to the current authenticated owner message, and link the subsequent outbound commitment to that record. Third-party replies and internal task events cannot create owner authorization. Interpretation of the owner’s words remains the LLM’s responsibility.
- `attach_task_reply`: the LLM associates a normal, unquoted reply with an incoming inquiry. Rust transfers only that authenticated message, checks the channel, deduplicates the association and enqueues an owner task event. It suppresses an unnecessary private-assistant acknowledgement. Unrelated requests can remain private tasks.
- `schedule_task`, `cancel_task_schedule`: persistent one-shot wakes, between 60 seconds and 30 days. Production uses the Rust worker and wall clock; tests inject a timestamp. Wakes recheck task state, authority epoch and channels, and consume the existing daily work quota. Restart preserves pending schedules. Revocation, task closure and WhatsApp unlink prevent pending work from being sent.

Incoming participant messages run before internal task events so fresh replies are recorded before coordination resumes. A commitment request is idempotent per authorization; retries return the existing delivery state instead of sending another request. Pending events for one task coalesce. The next event includes a fresh authorized task snapshot so the model does not rely on stale conversational summaries. Coalescing changes processing frequency, not the task's message history.

Exact `STOP` from a channel recipient closes their incoming task channels without involving the LLM. It has a bounded control lane even when ordinary inbound quota is exhausted.

## Deterministic boundary versus model judgment

Rust enforces identity, ownership, channel authority, allowed tools, work limits, revision conflicts, queue state and scheduled execution. Tool arguments cannot select another actor. Ordinary participants cannot read an owner's task or another recipient's messages. Permission administration continues to use explicit protected approval.

The prompt in `prompts/coordination.md` supplies generic harness guidance: derive relevant actions from the participant's objective and authorized evidence, retain task state, minimize unnecessary disclosure, distinguish intended actions from evidenced outcomes, and persist unresolved obligations. It does not prescribe negotiation, offer comparisons, provider selection, or an initial-discovery/booking sequence. Those meanings must come from the request and accessible domain content. The earlier shared prompt contained procurement-shaped guidance despite using generic nouns; the neutral-prompt experiment records that correction and retains its historical baselines.

**Important limitation:** understanding natural-language authorization and minimizing free-text disclosures are still model judgments. `send_task_message` accepts model-composed text; channel authorization alone cannot prove that every fact in that text may be disclosed. The owner coordinator sees task information from several channels and could include it in an inappropriate draft. Removing broad private history and separating participant access reduces exposure but does not constitute an information-flow proof. The live test checks selected synthetic disclosures, not all possible leaks or prompt injections.

Generic authorization and commitment records now bind messages to an authentic owner source. They cannot independently prove that a model interpreted natural-language limits faithfully, or prevent a model from disguising a commitment as ordinary text. No payment or purchasing tool exists. Stronger recipient-scoped draft provenance and adversarial semantic-authorization tests remain necessary before treating the feature as a hardened autonomous production workflow.

## Regression coverage

Deterministic tests cover owner/participant isolation, forged actors and extra tool arguments, stale updates, idempotent reply attachment, staged sends, revoked channels, timer restart/one-time firing/cancellation, revoked authority, autonomous quotas and a scheduled wake executing through the real runtime handler. Existing permission, attachment and pair-conversation tests remain enabled.

The paid live lane runs a configured real model (Grok 4.7 and Astra 6 have passed) with the actual Rust intake, storage, tool execution and queues. WhatsApp recipients are deterministic fixtures. Each attempt retains its transcript, model trace, runtime activity and acceptance results, including failures.
