# Architecture decisions and implementation readiness

Current-policy update (v2026.09.26-17): one or more admins are supported and any configured admin may explicitly approve any valid request, including their own. Earlier independent-review/self-approval-denial recommendations below are historical and superseded by [the current approval policy](permissions-pilot.md#current-approval-policy-v20260926-17). Exact commands, actor authorization, expiry, staleness and replay protections remain required.


Consolidated 2026-09-26. This document distinguishes accepted direction from recommendations still requiring a precise contract. It does not claim the coordinator is implemented.

## Accepted decisions

| Area | Decision and detailed specification |
|---|---|
| Product | Carbon is a domain-neutral Rust harness on Reefy. Domain meaning lives in instructions/resources. The [Aventura Bay example](aventura-bay-example.md) includes recall, community guidance, vetted providers, outreach, negotiation, booking, payments and feedback. Those are intended product scope, not existing integrations. |
| Model boundary | LLM interprets, retrieves, reasons and proposes typed operations. Rust supplies verified identity and enforces permissions, budgets, validation, persistence and execution. No model shell, arbitrary process/code/SQL/path/HTTP, OS cron or executable uploaded skills. |
| Model integration | Prefer Reefy's existing LLM proxy; discover models from the proxy. Carbon owns prompt assembly, the bounded tool loop and recipient-safe context. Validate actual tool/vision/schema behavior and an upstream route suitable for this shared workload. |
| Prompts and bundles | Small versioned core prompt plus applicable authorized instruction revisions, current event, bounded recent history/work notes and permitted evidence. Bundles are live collections created/edited through WhatsApp, potentially empty initially. Reference content cannot become trusted instructions through ordinary file-write rights. |
| Identity | Stable random participant UUIDv4; verified scoped WhatsApp bindings are replaceable. First admitted live contact can create an identity, never administrative/group authority. Reviewed verified relinking preserves ownership/history. |
| Permissions | SQLite authority; Rust checks every operation. Explicit owner and participant/group grants; additive folder inheritance, no implicit deny/override. Readable to X does not imply readable to Y. Read permission for a recipient allows sharing to that recipient; no independent forward ACL. |
| Disclosure | Private planning and destination-scoped generation are separate runs. Shared writes, messages and external queries enforce source restrictions. Explicit publication requires authority over all restricted sources, not just ownership of the output. Reply audiences come from generic coordination scopes and participant/conversation relationships; sponsorship alone grants no read access. |
| Permission experience | Ordinary coordination starts from the participant's request with zero permission prompts inside enabled capabilities. Generic scopes provide audience/capability bounds; the LLM interprets domain meaning. Contractor quoting is one example, not a core workflow type. |
| Administration | Natural language proposes group/membership/capability changes. Protected changes require backend-enforced 2PR from a distinct eligible participant; capability use does not confer grant authority. Issuer/requester/reviewer separation for adopted requests is an explicit pending role-policy gate. True admins originate in protected configuration and verified enrollment. |
| Approval syntax | Case-insensitive exact commands such as `approve A42` / `approving A42`. Bare approved verbs only when reply-bound to the stored approval message. No fuzzy “yes,” forwarded/history approval or LLM authorization. Immutable operation, expiry, replay prevention and execution-time checks. |
| Storage | [SQLite + private local Git/LFS](content-storage.md). SQLite owns operational state and published resource/version references; Git versions user/Carbon resource bytes; local LFS holds large bytes. No ACL sidecars. Stable UUID paths; immutable revisions; only Carbon mutates managed content. |
| Layout | All persistence in one backed-up /data volume. /data/state: config, secrets, SQLite, WhatsApp protocol state, staging, derived data, retained logs. /data/content: resource repository including .git and local LFS objects. Code/libraries stay in image. |
| Publication | Persist Git/LFS objects and retention refs first, then reauthorize and publish references/metadata/audit/outbox atomically in SQLite. Git HEAD is not authority. Orphans remain inaccessible; missing/corrupt metadata fails closed. |
| History | Current authorization plus version visibility; granting current content does not expose private predecessors. Content rollback publishes a new version and never rolls back permissions or actions. |
| Retrieval | Exact lookup and scoped SQLite FTS5 first; captions/OCR/transcripts retain source links. Vector retrieval is optional after measured need. Search, snippets, history, derivatives and model context obey the same authorization. |
| Background work | Durable SQLite inbox/outbox and app scheduler using bounded Tokio tasks. No OS cron. Recheck authority at execution and dispatch. Leases/retries/idempotency and ambiguous-send recovery; no exactly-once claim. |
| Transport | Direct pinned oxidezap/whatsapp-rust linked-device adapter. No initial public webhook or Node bridge. Baileys only as contingency; official Cloud API/public ingress is a future adapter. |
| UI and packaging | Concise Rust-served setup UI: WhatsApp pairing, proxy model choice, true-admin setup. Optional explicit password auth; disabled only on loopback. Shared web password is not reviewer identity. CamAdmiral Docker/Reefy lifecycle patterns. |
| Backup and testing | [Whole-volume backup/restore](backup.md), no exclusions; Git clone/push alone is insufficient. [Unit and especially E2E tests](testing.md) accompany every feature, with real authorization/storage and deterministic external adapters. |
| Deferred/declined | Application encryption/key management deferred; SELinux-specific integration declined. No inode identity, xattr ACLs or filesystem watchers initially. No automatic destructive Git/LFS pruning. |

See [alternatives and research lessons](architecture-alternatives.md) and [memory research](memory-research.md). These decisions supersede earlier plain revision-directory and possible ACL-sidecar proposals.

## Abuse protection and future integrations

Per-participant and installation-wide abuse protection is required from the foundation, with administrator-configurable defaults enforced by Rust. See [budgets, proposed pilot limits and bounded internet research](abuse-and-integrations.md). External research is planned for later; the capability contract must support scoped disclosure and cost accounting now. Numeric defaults are proposals for pilot calibration.

See the [architecture review](architecture-review.md) for additional schema/run-contract gaps, ranked by when they must be resolved.

## Are we ready?

Ready to implement the foundation and integration experiments; not yet ready to freeze the full coordinator schema/tool API or launch autonomous coordination. The deeper [architecture review](architecture-review.md) found missing disclosure rules for shared writes/derived publication, reply-sharing semantics, authority composition and trusted LFS ingestion, in addition to the gates below. Resolve those with executable examples first; the major stack choices remain settled.

These are proposed defaults, not silently accepted changes to scope:

| Design gate | Recommended starting contract | Why settle before implementation |
|---|---|---|
| Installation and admission | One independent installation/security realm per Reefy app volume; many participants and groups inside it. Invite/admit participants explicitly. Initial conversations are direct messages; reserve group-chat support until sender-versus-audience semantics are tested. | Avoid confusing a WhatsApp group with Carbon membership or treating unrelated organizations as one realm. Preserve installation/account scope in identity bindings. |
| Version visibility and deletion | Additive grants as already selected; explicitly record which versions were published to each audience. New grants start with the current published version and later shared updates, never all predecessors. Historical reads require both current read access and version eligibility. Retain versions initially; deletion means hidden/tombstoned until a purge/backup-retention contract is approved. | A simple resource-level ACL alone cannot express private drafts becoming shared. Removal is not erasure from Git/LFS or backups; set quotas and reject writes before disk exhaustion. |
| Delegated action authority | Separate permission to read, permission to contact someone and authority to make a commitment. Store scoped delegation with recipients, operation classes, expiry and limits. Default to exact customer approval for binding booking/payment commitments; negotiation and follow-ups can proceed within explicit delegation. | A safe context does not prevent an LLM from inventing promises. Pure free-text messages cannot deterministically guarantee absence of commitments. Use canonical confirmation records/messages for binding actions; require review of unrestricted drafts where that guarantee matters. |
| Bootstrap and recovery | Protected config enrolls at least two distinct reviewers through verified endpoints. Web operator can initiate setup but cannot impersonate them. Define a device-owner recovery procedure that pauses protected execution, records the intervention and re-verifies bindings. | Avoid lockout, self-approval loopholes and takeover via recycled phone numbers. Finalize the exact protected-operation matrix, including ownership transfer and policy/instruction authority. |
| Instructions and run isolation | Core safety rules always win. Resolve deterministic scope/order/activation conflicts before running; do not claim Rust can prove natural-language instructions semantically consistent. Record versions supplied, cap tool iterations/tokens/time and prohibit carrying private free text into broader-audience drafting. | Prevent cross-work-context leakage and instruction ordering becoming hidden policy. Test model input as well as output. |

The intended contractor example still includes booking and payments. Choose provider, supported regions, confirmation/settlement semantics and idempotency behavior before implementing a payment adapter. This does not block identity/storage work or outreach with explicit authority.

## Technical validation gates, not product redesign

- **Git/LFS:** select a Rust library or fixed backend-controlled integration; prove offline pointer/object interoperability, durable publication, retention refs and crash recovery. No hooks, ambient configuration, automatic remote fetch or arbitrary commands. Establish deterministic file-size/type routing and upload quotas.
- **WhatsApp:** pairing, restart, LID/phone identity mapping, quote/reply approval binding, reconnect, media, duplicate/history/echo handling and ambiguous sends with the pinned library. Measure on the current x86-64 target; mock protocol tests alone are insufficient. ARM64 is deferred.
- **LLM proxy:** verify real model discovery, tool-call correlation/schema handling, image input, cancellation/timeouts and usable route credentials. Provider compatibility and subscription suitability remain checks, not assumptions.
- **Backup:** stop/copy and actual Reefy snapshot/restore tests covering SQLite WAL, protocol state, all referenced Git/LFS objects and stale queued work. Known restores quarantine normal participant reads/model access and external effects until reconciled; takeover must fence the old instance.
- **Migration:** move current starter config/secrets into /data/state explicitly; preserve credentials, detect conflicts, keep failed migrations from masquerading as a fresh install.

## Implementation order and completion evidence

1. Establish deterministic application E2E harness, data layout migration, installation identity and persistent quota/admission primitives. Keep existing starter checks.
2. Implement participant/resource IDs, migrations, authorization matrix and Git/LFS publication. Prove owner upload/read, outsider denial, restart and crash recovery with real storage.
3. Implement enrollment, groups, 2PR and exact version publication/sharing. Prove revocation, replay/self-approval denial and history isolation.
4. Add inbox/outbox, native transport and setup UI pairing. Prove actual live pairing separately from deterministic CI.
5. Add proxy model discovery, prompt/context assembly and scoped tool loop. Assert private canaries never enter another audience's model requests or payloads.
6. Add durable follow-ups, live bundles and first configured end-to-end journey. Expand to a second unrelated example to test the generic core. Add booking/payment capabilities only with their authority/provider contract and regression tests.
7. Validate Linux x86-64 and Reefy install/update; complete a real backup/restore drill before production use. ARM64 is not a current requirement.

Each increment includes unit, integration and E2E tests; these are not a final phase. Current implementation is only the authenticated setup shell and Docker packaging with Linux amd64 CI. Coordinator permissions, Git/LFS persistence, WhatsApp, LLM integration and the above journeys are not implemented or validated yet.
