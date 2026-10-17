# Architecture review: implementation gates

Current-policy update (v2026.09.26-17): one or more admins are supported and any configured admin may explicitly approve any valid request, including their own. Earlier independent-review/self-approval-denial recommendations below are historical and superseded by [the current approval policy](permissions-pilot.md#current-approval-policy-v20260926-17). Exact commands, actor authorization, expiry, staleness and replay protections remain required.

**Verdict:** Rust, SQLite authority and private local Git/LFS remain a workable stack. Foundation experiments can start, but the previous “ready” assessment was too broad for freezing the coordinator schema/tool API. The gaps below need explicit contracts and executable examples first. Documentation corrections clarify existing privacy and authority constraints; proposed product defaults remain identified as proposals.

## Follow-up: generic core and approval burden

The user requires **no permission questions during ordinary authorized coordination**, including its first request. Contractor quoting is one example configuration; Carbon's core does not know that meaning. This supersedes the earlier suggestion to agree an inquiry's sharing scope once with the customer. The findings below describe backend checks, not customer approval steps. Apply [the permission experience contract](technical-guide.md#permission-experience-ask-only-when-authority-changes): reuse standing authority and agreed audiences, do routine work silently, and ask only for a necessary new scope or protected change.

Configured generic intake scopes and participant/conversation relationships supply bounded input/reply audiences. The LLM interprets intent and proposes a context; Rust checks existing authority and input provenance. Customer requests and factual/photo responses then produce quotes without a permission setup step. Unrelated private history stays outside outbound context; gather current evidence or omit unavailable material instead of asking to release it. Explicit private excerpt publication remains a separate operation, not a prerequisite for quotes. Ordinary work contexts must not create administrative groups and trigger 2PR. Denials do not become admin escalations.

## Findings and corrections

### R1 — All model outputs need disclosure checks

**Before resource/tool APIs.** The old plan protected message drafting but allowed shared-resource edits with only write/version checks. A private run could copy a private canary into a community note, title or scheduled instruction, after which a recipient-safe message run could read that now-shared note.

Apply runtime-recorded source/audience restrictions to every persistent or external output, including metadata. Stage more-restricted drafts privately or regenerate with only destination-readable context. Source lists come from the runtime, never model citations. This is enforcement of the existing recipient-read rule, not a new forward permission. See [information flow](identity-permissions.md#information-flow-and-publication) and the corrected tool guarantees in the README.

Ownership of a generated summary is also insufficient publication authority: someone who can read another person's private source cannot publish it simply by creating a new file. Exact publication needs authority to release every restricted source involved. Explicitly released versions and ordinary source-restricted derivatives must be distinguishable.

### R2 — Define whose conversation a reply belongs to

**Resolved product direction; specify generic scope binding before inbox/message schema.** Configured intake rules and participant/conversation relationships establish bounded audiences. Verified correlation selects an existing context; otherwise the LLM proposes a scope already invocable by the participant. Rust checks source provenance and recipient/capability bounds, not business meaning. Model selection cannot broaden originally private inputs.

Route covered replies/attachments using the context audience without permission prompts. Interpreting ambiguous prose belongs to the model; uncertain correlation can prompt a normal factual question. Do not hardcode a quoting state machine or provider role into the core.

Test two simultaneous inquiries to the same person: a reply cannot become visible to both requesters. A quoted message ID selects only a context the verified sender may participate in.

### R3 — One bounded authority set per run

**Before delegation and 2PR schema.** Sender, executor, sponsor, grant issuer and reviewer have different roles. A malicious counterparty reply must not execute arbitrary commands as the sponsor or spend an unlimited sponsored budget.

The run envelope needs installation, work-context ID/revision, event identity, execution principal, delegation ID/version, recipient/destination scope, source context and budget owner. Effective authority is the executor's current rights restricted by its delegation, not a union of the sender's and sponsor's rights. Incoming counterparty text is input to that bounded continuation, not fresh sponsor consent.

For protected changes, distinguish a general request from a proposal whose issuer has authority for the complete effect. Independent approval is an additional gate, not a way to assemble missing privileges from several partial principals. Recommended default: issuer and requester coincide for executable proposals; an administrator adopting a general access request becomes its new issuer and needs another eligible reviewer. Reviewers cannot be that issuer or the original requester. Otherwise a nominally different requester can conceal self-approval by the administrator making the actual grant. Finalize this operation/issuer/reviewer matrix before migrations; the stricter adopted-request flow remains a proposed product policy.

Canonical review messages are disclosure outputs too. True-admin eligibility alone does not permit private names, documents or instruction text. Give the reviewer sufficient authorized detail or route to an appropriate reviewer; do not silently grant blanket read access or approve an opaque operation.

### R4 — LFS pointers supplied by users are not authority

**Before first content ingestion.** Git LFS clean preserves an already-formed pointer ([official implementation](https://github.com/git-lfs/git-lfs/blob/main/commands/command_clean.go)). An importer that trusts this output and merely checks local-object availability could register another participant's private bytes as a new uploader-owned resource.

Hash/count actual admitted bytes. Record backend-selected storage kind and payload checksum/size; only the backend creates trusted pointers. Pointer-looking uploads remain literal or are rejected, never implicitly dereferenced. See [storage ingestion rule](content-storage.md#uploaded-bytes-are-not-trusted-storage-references).

Test a forged pointer whose hash references existing private media, not only missing objects. The allowed result is literal content/rejection with no private read, thumbnail, index or model context. Knowledge of a hash is not authorization.

### R5 — Old backups restore old permissions too

**Before restore automation/live release.** Pausing sends and jobs is insufficient: an old ACL or phone binding could immediately expose data through a read, search or model call. A /data lock also cannot stop another restored copy on another device using the same transport identity.

Known restores must quarantine normal participant access and transport activity until trusted recovery reconciles grants/bindings, pending effects and budgets. The deployment must signal restore outside the restored snapshot; local old state cannot detect changes made after that snapshot. Takeover must fence the old instance or revoke/re-link sessions, not merely generate a new local epoch. See [restore contract](backup.md#restore-contract).

No external revocation journal is selected. The initial recovery process must conservatively revalidate authority and acknowledge what an old snapshot cannot reconstruct.

### R6 — History eligibility must survive revoke/regrant correctly

**Before grant/version schema.** Share v1 with G; revoke G; keep v2 private; grant G anew at v3. A check combining current read permission with an old publication to G would expose v1 despite the new grant starting at v3.

Use grant/publication epochs or equivalent explicit ranges, including inherited grants and moves. New membership in a continuing group is distinct from a new grant to that group. Recommended group behavior is access to history intentionally published to the continuing group; private predecessors stay private. That group-history product default still needs confirmation.

### R7 — Persistent jobs do not by themselves bound autonomy

**Before scheduler and autonomous replies.** An admitted counterparty can repeatedly wake a sponsor's agent; two agents can reply to each other indefinitely; one-time jobs can create successors forever. Per-turn/day quotas limit damage but do not define the lifetime of authorized work.

Require expiry and cumulative budgets per delegation/work context, inherited by all descendant jobs. Add remote-sender trigger limits and coalesce pending wake-ups. Replies and successors cannot renew sponsorship. The [proposed defaults](abuse-and-integrations.md#proposed-default-policy) now include lifetime/trigger limits.

Separate work-context, run, scheduled-job and external-attempt lifecycles. Lease generations and expected context/cancellation revisions must fence stale workers, late model completions and outdated send requests. Two participants can race the same context even with one active turn per participant. Do not hold DB transactions across model/network calls.

An admitted external attempt remains recorded if cancellation arrives later. Uncertain delivery is not proven failure; retries require reconciliation or provider idempotency, and no exactly-once guarantee is claimed.

### R8 — Domain claims must match deterministic capabilities

**Before the first autonomous journey.** The example promises vetted-only outreach, verified reviews and bounded commitments while storing their meaning in model-editable Markdown. A schema validates shape, not the truth or authority of these claims.

The model can recommend based on evidence. Rust can enforce a generic administered recipient allowlist/delegation. Trusted verification claims need attributable human or reviewed backend attestations; model text cannot create them. Uniqueness of a verified review needs an actual reviewed invariant before being promised. Keep the domain-neutral core, but do not market advisory reasoning as a hard gate.

Arbitrary free-text negotiation may imply a promise even with a price limit in the prompt. For strict guarantees use exact reviewed outgoing text or a structured reviewed commitment capability. Booking/payment execution remains gated on provider and consent semantics.

The example's unqualified Cloud API templates/windows were also stale relative to the native linked-device decision; they are now explicitly future-adapter mechanics.

## Additional implementation refinements

- **Instruction consistency:** Rust enforces explicit scope/order/activation rules; it cannot prove arbitrary prose semantically consistent. Keep trusted-but-confidential instruction content subject to disclosure checks.
- **Research destinations:** community-shared does not mean public-internet-shareable. LLM processor, search provider and fetched origin have separate disclosure policies. Internet integration remains deferred.
- **Abuse controls:** exact cancel/opt-out/emergency commands need a bounded path before ordinary conversational quota rejection. Natural-language interpretation alone cannot work after model quota exhaustion. Global/provider outages can still prevent processing.
- **Storage accounting:** logical participant/sponsor charges and physical disk reservations are separate. Include staging, worktree/LFS copies, derivatives, metadata and orphaned writes. Settle publication and accounting together; ownership transfer or tombstone does not erase retained-byte debt.
- **Selective purge:** retaining shared Git history can retain deleted bytes. Future per-user erasure may need history rewriting plus SQLite reference remapping and backup policy changes. If near-term erasure is required, settle it before fixing commit topology; otherwise retain the explicit tombstone-only pilot contract.
- **Configuration/migrations:** one authoritative setting for each live policy; trusted startup config must not overwrite reviewed runtime changes. Serialize schema migrations and reject unsupported/partial state. An older image is not automatically a valid rollback.
- **Observability:** operator diagnostics must show operation IDs, denied reasons, reservations and uncertain outcomes without private transcripts. Source provenance is sensitive metadata too.

## Decisions needing product input

1. **Generic reply sharing is decided:** configured intake scopes and participant/conversation relationships provide bounded audiences automatically. Implement generic scope/correlation validation; no domain-intent parser or per-request agreement.
2. **History semantics:** whether a new member sees prior history published to a continuing group grant; fresh resource grants remain distinct.
3. **Autonomy level:** free-form delegated negotiation with semantic risk, or exact human-reviewed messages for commitments. This affects experience, not the Rust/storage choice.
4. **Erasure timeline:** tombstones/retained history are sufficient for the pilot only if no near-term selective-purge promise is required.
5. **Adopted access requests:** whether an administrator turning an ordinary user's request into a privileged proposal needs a second eligible administrator to review it. The recommended issuer/reviewer separation requires this; do not silently treat the ordinary user's request as the administrator's independent review.

Existing proposed defaults for invitation-only admission, direct messages first, two verified reviewers and scoped owner recovery remain recommendations. No new product acceptance is inferred from this review.

## Implementation and test gate

Before freezing schema/APIs, write executable contract examples for R1–R4 and R6: shared-write leakage, mixed-owner summary publication, reply audiences, issuer/reviewer authority, forged LFS pointers and revoke/regrant history. Add stale-worker/cancel, reply-loop and restored-revocation tests as the related mechanisms land. These are necessary behavioral regressions, not line-coverage targets.

Start implementation with the deterministic E2E harness and private upload/read/outsider-denial slice, including interruption and restart. Run the Git/LFS and native WhatsApp/proxy compatibility experiments early. Continue to sharing/2PR, then bounded LLM runs and scheduling. Live autonomous outreach waits for these contracts and regressions; restore/device acceptance gates release.

Avoid adding a generic policy language, workflow engine, unrestricted plugin system or new database. Narrow generic capabilities and explicit typed records are enough for the accepted scope.
