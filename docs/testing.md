# Testing and security regression requirements

Current-policy update (v2026.09.26-17): one or more admins are supported and any configured admin may explicitly approve any valid request, including their own. Earlier independent-review/self-approval-denial recommendations below are historical and superseded by [the current approval policy](permissions-pilot.md#current-approval-policy-v20260926-17). Exact commands, actor authorization, expiry, staleness and replay protections remain required.


Accepted engineering requirement, 2026-09-26: unit tests and especially end-to-end tests are first-class deliverables from the first feature. Add tests with each feature; do not postpone authorization coverage to a final hardening milestone.

## Definition of done

- New behavior has meaningful unit/integration coverage for its rules and a real application E2E scenario for its user journey.
- Protected behavior has both allowed and denied cases, including cross-participant access and no unintended side effects.
- A bug fix includes a regression that reproduces the failure. Prefer behavior assertions over implementation-mirroring tests or line-coverage targets.
- Changes to identity, sharing, ACLs, inheritance, 2PR, history, search, jobs or storage run the applicable security regression suite.
- Required CI checks pass before release/merge. Never turn a failed security test into a silent skip or mask flakiness with unconditional retries.
- Document any external integration or device acceptance that has not actually been exercised.

## Test layers

1. Rust unit tests: identity resolution, permission decisions, inheritance, schema validation, approval parsing, deadlines and state transitions. Table-driven authorization matrices cover actor, audience, operation, resource version and policy state.
2. Storage/integration tests: real temporary SQLite and filesystem state, constraints, migrations, concurrent operations and recovery. The accepted Git/LFS content store must use real local Git objects/refs and LFS storage in tests rather than mocked publication; SQLite remains permission authority.
3. Application E2E: start the real Carbon binary/container with isolated temporary /data; inject transport events through a test-only adapter; run production identity, authorization, persistence, orchestration, scheduler and outbox code. Capture outbound messages and model requests at external boundaries.
4. Browser E2E: test actual configuration/authentication and later setup workflows against the running app. Check authorization even when UI controls are bypassed by direct requests.
5. External acceptance: dedicated test-account WhatsApp pairing/reconnect/media and actual Reefy installation/update/restore checks. Report separately; simulated transport tests do not establish provider compatibility.

Use deterministic fake LLM/protocol services for the default CI E2E suite, not real model answers or production accounts. Never mock the authorization decision or persistence inside that suite. Test-mode controls must not be reachable/enabled in production. Live-model evaluations measure conversation quality separately and never determine whether a permission test passes.

## Required journeys and denial cases

Use synthetic participants Alice and Bob, an explicitly shared group, and two distinct reviewers. Give private resources unique canary strings so leakage assertions can inspect model input, search results and outbound payloads.

| Scenario | Required assertions |
|---|---|
| First contact and relinking | Repeated/concurrent events reuse one identity; new contact gains no admin/group rights; verified reviewed relink preserves UUID and retires old authority. |
| Private upload and recall | Owner can retrieve content; another participant cannot retrieve by guessed ID, path, search, preview, history or export; no private canary reaches their model context. |
| Explicit sharing | Intended audience can read; outsiders cannot; grant does not expose private earlier revisions or unrelated children. |
| Folders and moves | Additive inherited grants have defined results, with no deny/override semantics; destination checks prevent accidental publication; rename preserves identity. |
| ACL failures | Missing/corrupt/unsupported policy or database failure denies access; ordinary uploads cannot replace policy. |
| 2PR | Unapproved, self-approved, expired, replayed or ambiguously bound responses cannot change rights; case-insensitive exact approved commands work; authority is rechecked at execution. |
| Revocation | Queued jobs, drafts and caches cannot restore withdrawn authority; use controlled barriers to test races at the final dispatch boundary. Already dispatched external messages cannot be recalled. |
| Recipient-safe generation | Outbound model input contains only material allowed for all recipients; injection in reference files cannot bypass tool checks or edit policy. |
| Concurrent participants | Interleaved async work keeps actor, scope, caches, summaries and outbox recipients separate. |
| Delivery/retries | Duplicate inbound events are idempotent; crashes around dispatch follow documented retry/ambiguity behavior without assuming exactly-once external delivery. |
| Git/LFS isolation and retention | Missing LFS bytes deny reads; unregistered commits/raw history cannot bypass SQLite; pruning retains published and pending versions; restore verifies the entire object set. |
| Storage interruption | Faults before/after content persistence and metadata publication leave either valid state or unavailable/quarantined content, never wider access. |
| Restore/update | Restore real saved state, validate private/shared/history access and schema migration; old jobs/grants do not resume unchecked; secrets are retained without test logs disclosing them. |
| Resource limits | Oversized uploads and bounded work/model-call budgets cannot starve other participants or bypass authorization. |
| Shared-write disclosure | A private canary cannot be copied into a shared document, title, bundle instruction or scheduled-job text; write permission is not publication authority. |
| Mixed-source publication | Owning a summary does not allow releasing another owner's restricted source; explicit source-authorized publication works for the exact output/audience. Later copies/moves/grants cannot expand that release audience without new applicable authority. |
| Reply sharing and sponsored authority | A private or ambiguously correlated reply is not forwarded; accepted inquiry audience permits only its declared recipients. A hostile counterparty cannot expand sponsor capabilities or budget. |
| Forged LFS pointer | A pointer-shaped upload naming another participant's existing object is literal or rejected; it never yields those private bytes or derivatives. |
| Grant generations | Revoke/regrant starts the intended new visibility range; joins/moves/ownership changes do not resurrect private versions. |
| Review disclosure | A true admin without source visibility cannot receive private filenames/content through canonical approval summaries; adopting someone else's request does not let an issuer self-review. |
| Worker fencing and loops | Late model results and expired lease holders cannot mutate/send after cancellation or a newer decision; counterparty floods and successor jobs consume finite inherited budgets. |
| Restore quarantine and takeover | Old grants/bindings cannot serve any participant reads/model context before recovery activation; separate restored volumes cannot both be activated through the supported takeover procedure. |

## Domain-neutrality regression

Run two unrelated example configurations through the same orchestrator, authorization/scope service and tool schemas: contractor inquiries and, for example, coordinating a community event. Change only instructions/reference resources, participant profiles and generic scope configuration. No domain-specific Rust branches, tables, business-stage engine or tools are allowed.

Reject model proposals for an unavailable scope, out-of-scope recipient or historical private source mislabeled as fresh input. Accept permitted operations for either example through the same code. Semantic interpretation quality is evaluated separately: authorization/schema checks cannot prove the model understood the request correctly.

## Permission experience regressions

Permission safety and prompt count are both observable behaviors. For the contractor example, start with generic deployment intake/scope configuration, an admitted initiating participant and allowed counterparties; create no per-inquiry grants, delegation or approval fixture. From the first “my AC is broken” message, through factual questions/photos, provider outreach, relayed quotes and follow-ups, the real backend must instantiate the bounded context automatically and emit zero permission-confirmation requests and zero admin-review requests. No first-request consent screen or brief approval is allowed. Factual task clarification is permitted; assertions must reject permission questions even when phrased as clarification.

Also assert:
- A clear authorized share request executes without asking the requester to repeat it.
- Draft/summary revisions made only from audience-readable sources reuse authority; exact release of a private excerpt asks once and can then be reused.
- An unrelated denied read or private historical attachment is omitted/handled without owner/admin escalation; Carbon may ask for current factual information/photo instead. Private canaries cannot appear in provider requests, while current-inquiry photos are delivered to the selected eligible recipients without extra approval.
- An unsupported disclosure is omitted or handled with permitted current-inquiry inputs; it cannot introduce an approval step into quoting. Separate user-initiated administration/publication can produce one deduplicated concrete approval request; retries and scheduled wakes cannot multiply it.
- Protected administration still requires 2PR; one explicit batch shows every change and applies atomically only if all checks pass.
- Revocation/expiry never causes silent renewal. A pending question or absent response cannot become a grant.
- Standing reply policy applies to the correct inquiry requester and respondent only; no cross-customer or competing-provider leakage. Only selected recipients receive request inputs, not the entire eligible pool.
- Model-supplied resource IDs cannot relabel historical/private content as fresh inquiry input or exceed the configured recipient pool. Automatically created context grants remain bounded by the current policy and cannot trigger 2PR merely because they create permission records.

## Abuse and integration regressions

The [abuse/integration test matrix](abuse-and-integrations.md#required-tests) is required as these features land. Cover concurrent quota reservation, restart/relink persistence, sponsor attribution, installation caps, disk exhaustion and bounded control operations after conversational quota exhaustion. Future web research requires SSRF/redirect/rebinding denial, query-disclosure canaries and hostile-result tests. Use deterministic fake external services while retaining real policy/accounting enforcement.

## Harness and CI

- Use injectable time and seeded inputs; use barriers/events for race tests rather than arbitrary sleeps. Bound waits and report useful errors.
- Every test owns unique directories/containers/volumes and cleans up only its fixtures. Never use production data or credentials.
- Unit and application E2E tests run on every pull request and push. Expand the existing container-check workflow as features land; enforce required checks through repository protection when that is configured.
- Keep a fast deterministic security suite in the normal gate. Longer fault-injection, migration and recovery suites are release gates; external acceptance uses explicitly provisioned test environments.
- Failure artifacts include sanitized logs, operation IDs and test seed. Do not upload credentials, private transcripts or unrestricted data volumes.
- Keep tests expressed in public behaviors so the same permission suite verifies SQLite authority across current and historical Git/LFS content.

## Current coverage and next implementation steps

The starter currently runs cargo test, cargo clippy and tests/container_smoke.py in GitHub Actions. The container test covers actual HTTP authentication, non-root startup, SIGTERM and credential persistence across recreation.

Participant ACLs, WhatsApp transport, LLM tools and bounded pair coordination now have synthetic integration and mock-provider tests. The full journey matrix above remains a target; scheduling, restore fencing and broad task orchestration are not implemented. Expand the harness with each feature rather than claiming container smoke covers tenant isolation. Branch-protection enforcement and new test suites are not configured by this document.

Approval regressions cover quoted reply binding to the exact notification and recipient, wrong/forged/forwarded replies, case-insensitive self-approval, quote persistence across restart, duplicate decisions, safe legacy notice migration, UI text-only rendering and protected request-list access. The mock-LLM sharing journey now approves via the persisted WhatsApp quote binding before checking recipient access.

Versioned-note tests include real concurrent writes, same-update retries, stale bases, owner-only edits, hidden history, inherited-folder isolation, retained external provenance after revision, LFS/legacy restart migration and injected SQLite publication failure after durable Git writes. The mocked model E2E exercises read → revise → history → durable reply with forged actor/path and cross-participant denial checks.

Approval-admin management tests cover current-role enforcement, group-admin separation, exact target identity, removal/re-add generations, independent additions, concurrent last-admin protection, revoked quotes, valid pending-request handoff, uncertain-notification non-replay, sealed bootstrap across restart, role-change UI refresh and unchanged private-content access. A mock-model E2E requires a real transport-style quoted human approval before the proposed promotion has any authority.

WhatsApp linked-device intake regressions and live acceptance are documented in [whatsapp-linked-devices.md](whatsapp-linked-devices.md). The parser/production-handler/SQLite/HTTP Activity journey runs with `cargo test --locked`; live phone/Mac delivery is explicitly separate from the synthetic application E2E.

## Scoped coordination

`src/coordination.rs` contains synthetic scope/queue authorization tests and a mock-HTTP-model E2E. The latter deliberately gives the private planner a private-history canary and lets it retrieve a private note; neither may appear in the isolated pair model call or outgoing payload. Other cases cover parallel pairs, quoted routing, forwarded input, unchanged file ACLs, source-text forgery, scope/membership revocation and re-addition, identity changes, lifetime limits, reserved STOP, unlink and restart. These tests require no live WhatsApp account.

Run compilation, Rust/UI tests, clippy and container smoke on the authorized Linux build host, not on the editing Mac. Keep Cargo target/registry caches on that host and copy source only.

Scoped-resource application tests in `tests/app/scoped_resources_e2e.rs` exercise both-participant ACL intersections (including derived sources), exact fresh-upload admission without global grants, private/forwarded/wrong-recipient/text-only denial, reviewed scope upgrades, revoked history and pending-output exclusion, expiry during upload, uncertain-send restart, shared private/pair budgets and real Git/LFS bytes. Mock HTTP model journeys cover search → read → denied private send → authorized file dispatch, and provider-only PDF/audio interpretation with cleanup and pair provenance. An async image-provider test revokes authority before returning and verifies that no stale draft can be sent. All fixtures are synthetic; actual WhatsApp media transmission is a separate live acceptance check.

## Processing feedback

`tests/app/progress.rs` uses a deterministic clock and fake transport to check immediate typing, 10-second refreshes, one delayed notice, success/failure cleanup, dropped futures, revocation, disconnects, feedback failures and the overall processing deadline. SQLite tests check exact actor/destination binding, no progress-text insertion into model history, and durable unknown-send deduplication across restart. An HTTP mock holds a proxy response past the old 90-second deadline and verifies the actual Carbon client still accepts it. Scoped application tests cover replies to progress notices and immediate quoted STOP while model work remains active, including wrong-participant and forwarded denials. No live WhatsApp messages are sent by tests.

## Neutral live coordination scenarios

On an authorized Linux x86-64 host, `CARBON_E2E_LIVE=1 CARBON_E2E_SCENARIO=ac scripts/e2e-ac-live.sh` runs the 13-check AC journey. Set `CARBON_E2E_SCENARIO=event` for the independent reading-circle scenario; select Astra or Grok through `CARBON_E2E_MODEL`. Each run requires a fresh artifact directory and uses real paid model calls but simulated WhatsApp/people. Both exercise the actual Rust model loop and tools. The event scenario has eight checks, including private fallback limits, relevance, adapted duration, owner-authorized commitments and durable reminders. [Results and known grading limits](neutral-prompt-v2.md).

The deterministic malformed-task-argument test proves rejected empty JSON arguments create no task, can be corrected in a subsequent model round, and repeated malformed output stops at the existing ten-round budget. No task permission or authority check is bypassed.


## Dashboard navigation

`node tests/ui_state.cjs` checks sidebar state, combined Admin visibility, and the existing QR/admin/activity behavior. `tests/navigation_e2e.cjs` runs real Chromium against local static UI files and synthetic API responses. It covers default closed WhatsApp pairing, releasing pairing when changing pages, desktop collapse persistence, responsive mobile navigation, Escape and focus return, URL/history navigation, combined admin configuration/review visibility, and horizontal overflow. Screenshots contain synthetic data only. A separate `ui-navigation` CI job runs alongside the Rust/container job and retains its screenshots.

Run browser validation on an authorized Linux host with the pinned Playwright container and package version `1.56.1`; the job definition in `.github/workflows/container-check.yml` contains the exact command. `CARBON_UI_ARTIFACT_DIR` sets the screenshot directory. No live WhatsApp or model requests are made by this UI test.

The dashboard uses a left sidebar with a persistent desktop collapse preference and a mobile overlay drawer. Navigation links use `aria-current`; the toggle uses `aria-expanded` and `aria-controls`. Mobile content is inert while the drawer is open, and Escape/backdrop restore focus to the toggle. WhatsApp remains a closed-on-load detail inside Communication Channels. Admin configuration and approval requests share one Admin page. Only the visible page polls its detailed data.

Navigation references: [Carbon UI shell left panel](https://carbondesignsystem.com/components/UI-shell-left-panel/usage/) and [W3C disclosure navigation guidance](https://www.w3.org/WAI/ARIA/apg/patterns/disclosure/examples/disclosure-navigation/). The sidebar uses ordinary navigation links and native buttons rather than an ARIA menu widget. Skip-to-content moves focus without changing the selected page.

The desktop collapse control is a bordered button in the bottom-left sidebar footer, separate from menu items. It stays within the panel in expanded and collapsed states. Browser E2E checks the bottom position, separation from navigation items and clickable hit area. Mobile retains its header menu button.

The dashboard uses the Graphite + amber palette through CSS color tokens: neutral backgrounds/surfaces, amber active navigation and primary actions, and separate error colors. Browser E2E also checks body and selected-navigation text contrast (at least 4.5:1).

## Participant profiles

See [profile tools, permission regressions and live-model journey](participant-profiles.md#tests). `cargo test --locked` includes the production-model-loop mock E2E and fault-injection/ownership/visibility regressions. The optional real-model test is `live_profile_directory_journey`; it uses only synthetic participants and simulated WhatsApp, with model/tool traces saved into the requested fresh artifact directory.

## Concurrent processing

See [concurrency architecture and E2E coverage](concurrency.md). Run `cargo test --locked concurrency_e2e` on Linux for simultaneous participants, a gated slow provider, cross-user ACL rejection, ordered replies, scheduler progress, the processing bound, cancellation/restart and private/pair mutual exclusion.

## PDF provider regression

`analyze_document` sends authorized PDFs to the selected `codex/` or `openai/`
model as inline file content in an isolated, tool-free request. The existing
xAI upload path still requires an xAI-only proxy; speech transcription keeps
that same routing restriction. Unsupported providers produce a specific tool
error rather than suggesting a document upload will resolve the limitation.

Deterministic tests cover private and resource-enabled pair conversations,
selected-provider routing, source provenance, denied cross-participant reads,
revocation during analysis, rejected/truncated/action-bearing responses and
traces that reference the stored PDF without duplicating its bytes. They do
not establish compatibility with every upstream model.

Run the opt-in synthetic PDF acceptance against an authorized local DEV proxy
on Linux (no WhatsApp messages or customer documents are used):

```sh
CARBON_E2E_LIVE=1 CARBON_E2E_PROXY_URL=http://127.0.0.1:9080/v1 \
CARBON_E2E_MODEL=codex/gpt-6-astra \
cargo test --locked live_selected_provider_pdf_acceptance -- --ignored --nocapture
```

This exercises the production Carbon attachment handler with a three-page synthetic
PDF and checks facts from two pages. Run it with each intended provider/model;
API documentation alone is not a live compatibility result.

## Private attachment and reply continuity

`private_document_context_e2e` reproduces caption loss at the real decoded
WhatsApp intake boundary and the missing short-follow-up context in task mode.
Its deterministic journey covers direct/wrapped PDF uploads, immediate requests,
unquoted follow-ups, replies to the upload and to Carbon's own response, restart,
PDF analysis, immutable summary/source provenance, and pending read-only group
access proposals. Real approval checks reject premature summary grants and
stale source approvals; readers cannot edit the approved summary.

Private continuity uses at most eight completed, delivered turns from the past
30 minutes within a 20,000-byte context budget. Exact authenticated reply IDs
can select older turns. Sender identity, authority epoch and resource access
are checked again before reuse; resource dependencies remain subject to the
existing publication barrier. Task events and scoped pair planners do not
receive this private window. Turns that read a delegated inquiry are fetched
fresh through inquiry tools instead of replaying an exchange whose access
may have expired or been revoked. Context cannot authorize new contact or bypass
sharing review. Quoted message bodies supplied by WhatsApp are never used as
stored-message authority.

Normal Carbon replies now retain a stable provider message ID. IDs of replies sent
before this change cannot be reconstructed. Exact replies to older stored
uploads can supply currently readable file metadata, without replaying old
instructions or assistant interpretations whose provenance was not recorded.
Previously dropped captions cannot be recovered from the saved blank text.

The opt-in live test scripts one prior Carbon clarification to reproduce the failure,
then uses the selected real model for the short follow-up, PDF reading, saving
and read-access proposals. WhatsApp delivery and the document download remain
synthetic; no customer messages, documents or permissions are used:

```sh
CARBON_E2E_LIVE=1 CARBON_E2E_PROXY_URL=http://127.0.0.1:9080/v1 \
CARBON_E2E_MODEL=codex/gpt-6-astra \
cargo test --locked live_private_document_continuation_acceptance -- --ignored --nocapture
```

## Direct approval-admin document sharing

An authenticated approval admin can explicitly request read-only sharing of a fresh attachment or directly replied-to owned document, including a new summary saved in that turn. `share_requested_document` is offered only for that bound document scope. It is not a general permission or approval bypass. Non-admin sharing requests and other permission changes retain `propose_change` and request-bound review.

The tool has no recipient or original-document argument. Before the planner runs, Rust freezes the upload/reply binding, owner/content/ACL identity, authority epoch, and at most 32 current group identities/revisions. A separate tool-free model request sees only the fresh participant text, never document contents, filenames, group-directory labels, retrieved notes or previous conversation. Natural-language intent remains model interpretation; it is not a deterministic proof of semantics. Questions, quoted instructions, negations, exceptions and ambiguous audiences must not authorize sharing. Selected group names must occur literally in that message and resolve uniquely against the frozen directory. All-groups means those current group IDs, never Everyone or future groups. A failed interpretation is limited to one attempt per inbox; clarification requires a fresh request.

Rust rechecks admin/identity/ownership and the frozen scope after interpretation. Only the bound original and a note actually saved by this turn can be shared. The summary must have exactly that document as its source; other private sources cannot be silently exposed. A summary-only request cannot grant source access to satisfy that prerequisite. Grants are applied source-first in one SQLite transaction, with the existing access validators and authority barrier. Any error rolls back all grants. The result, audit record, current work/journal authority and execution checkpoint commit together; duplicate calls return the stored result. Current-turn provenance survives the barrier. Existing matching pending read requests are fulfilled and their unsent review notices canceled; unrelated requests are never approved.

`cargo test --locked` covers native WhatsApp upload/caption -> PDF analysis -> private summary -> eight read grants for four groups without notices, and the same planner attempting sharing against a negated request with zero grants. It also covers named-group resolution, ownership/source checks, atomic rollback, group/ACL/admin/identity changes, expiry/forwarding, bounded interpretation, duplicate calls, restart/checkpoint durability, and reconciliation of matching pending requests. Existing non-admin review/source-approval E2E remains in the suite.

On an authorized Linux x86-64 host, run the compiled test binary with `CARBON_E2E_LIVE=1`, `CARBON_E2E_PROXY_URL=http://127.0.0.1:<port>/v1`, and `CARBON_E2E_MODEL=codex/gpt-6-astra`, filtering `live_admin_document_sharing --ignored --nocapture`. The proxy URL must be a local DEV relay. Both live tests use synthetic stores/documents and simulated WhatsApp; no customer document or message is used. The full journey requires correct PDF facts, eight applied grants, zero approval requests, retained source provenance, read-only access and an unrelated note remaining private. Five request-only model checks cover negation, a question, quoted document instructions, summary-only access and an excluded group. These are explicit paid-model opt-ins and do not run in default CI. `CARBON_E2E_LIVE=1 CARBON_E2E_PROXY_URL=http://127.0.0.1:<port>/v1 CARBON_E2E_MODEL=codex/gpt-6-astra scripts/e2e-admin-document-sharing.sh` records both live tests and their exact source revision in a fresh artifact directory. The ordinary Carbon container-check workflow runs the deterministic sharing boundaries and upload → analysis → save → atomic sharing journey as a named synthetic E2E step on the existing self-hosted runners; the remaining Rust suite skips those filters to avoid duplicate execution. CI preserves the sharing logs and machine-readable result even on failure. The synthetic mode mocks the model boundary and does not claim real-model acceptance; the live mode requires a separately configured authorized DEV proxy.


### Document-only summary isolation after private history

The real DEV incident was reproduced before the fix by warming the upload/share journey with a private AC note and an older PDF upload, reading both, and retaining their conversation context. The previous `analyze_document` → planner `save_note` path inherited those private source dependencies and the unchanged atomic-sharing guard refused the whole batch.

`save_document_summary` now accepts only an authorized PDF resource ID. A tool-free provider request receives that PDF and a fixed document-summary prompt: no planner draft/question, private history, note contents, filename instructions or recipient directory. Its validated text is stored directly as a private Markdown resource with precisely the selected PDF as its source, in the same transaction as its durable per-inbox/source receipt and audit entry. Ordinary `save_note` and the mixed-source sharing refusal remain unchanged. Repeated calls reuse the stored result and recheck current identity/source access; provider failure or revoked work publishes nothing. This does not clean labels on existing mixed-source notes or change existing live grants.

The existing named sharing E2E now covers warmed private history and an older duplicate PDF, a malicious interpretation visible to the parent, a deliberately mixed planner-written summary that the sharing guard rejects, recovery by creating a new isolated summary without relabeling the old note, four read-only group grants for both resources, zero permission proposals, and another member's real runtime search → returned resource ID → read → answer. The negated-sharing case stays private and returns no search results. Four focused summary tests cover isolation, exact provenance, durable repeat calls, disallowed draft/actor/source arguments, unauthorized/non-PDF input, incomplete/action/refusal/empty provider output, stale authority and atomic SQLite failure. Both execute through the existing self-hosted CI; real-model acceptance remains an explicit opt-in and its fixture also warms prior private reads.

Upstream comparison informing the scope: [Hermes memory](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory) separates curated Markdown memory from SQLite session search; its documented [LLM Wiki workflow](https://github.com/NousResearch/hermes-agent/blob/main/skills/research/llm-wiki/SKILL.md) organizes immutable raw sources and linked Markdown pages. [OpenClaw builtin memory](https://docs.openclaw.ai/concepts/memory-builtin) indexes Markdown with keyword/vector retrieval. Carbon retains its existing immutable originals, source-linked Markdown resources, logical folders, SQLite ACLs and keyword index. Introducing embeddings or a wiki is separate retrieval work; neither corrects a summary that was generated with unrelated private inputs.


Message-note sharing regression: `cargo test --locked message_note` covers the isolated fresh-text editor, durable receipt persistence and repeat-call reuse, forged draft/identity fields, provider failure, stale work and write rollback. The named synthetic sharing script also runs `admin_message_note_sharing_e2e`: warm private history → save a synthetic resident list → follow-up read-only sharing (or ambiguous public-only wording → audience clarification) → another group member searches and reads the exact saved note. No real WhatsApp sends or customer contacts are used. Additional `direct_sharing_message_note` checks preserve the source guard, group snapshot, atomic grants and idempotency. Notes remain Git-backed Markdown with SQLite provenance/ACL metadata. Old mixed-source notes are not relabeled or granted access; resend the self-contained text to create a new isolated note. Public alone remains a clarification. Explicit Everyone/all-users requests use the verified-admin request-bound path and include future Carbon users; current-group wording never becomes an Everyone grant. Real-model acceptance remains pending.


Everyone-sharing regression: `admin_everyone_sharing_e2e` covers an admin saving and sharing with Everyone in one request, both the dedicated tool and the generic proposal fallback actually seen in the incident. The latter routes only an exact bound isolated note; fresh intent is checked independently, and the grant plus journal checkpoint commit together. Repeated work must not create duplicate effects. Current and future synthetic users can search/read the shared note but cannot edit it or read private history. Non-admin requests remain pending; negations/public-only ambiguity grant nothing; explicit requests for a proposal retain review. Additional `direct_sharing_everyone` boundaries cover source dependencies, ownership, role/identity/expiry changes, mixed audiences, partial-write rollback, exact pending-proposal fulfillment, and Everyone without expanding group membership (including zero or more than 32 current groups). Current-group sharing never becomes Everyone. No live customer messages or access changes are used by the tests.
