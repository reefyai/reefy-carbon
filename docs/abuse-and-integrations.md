# Abuse protection and future research integrations

Design requirements, 2026-09-26; not implemented. Rust enforces limits independently of prompts. Numerical defaults below are proposed pilot settings, not provider limits or measured device capacity.

## Abuse protection is part of the foundation

Count several resources, not just user messages: one message can trigger repeated model calls, downloads, fan-out messages, file revisions or scheduled work. Apply participant, work-context, recipient, provider-account and installation limits together. A permitted action still needs remaining quota.

Key participant accounting by stable UUID, not phone number; relinking does not reset usage. Before admission, use bounded provider-binding/account buckets and avoid model calls or media downloads. New identities must not provide unlimited access through repeated enrollment. Global limits bound abuse across many identities.

### Proposed default policy

| Dimension | Pilot default |
|---|---|
| Inbound live message admission | Token bucket per participant: capacity 10, refill 1 token per 6 seconds; count every unique event, including those not requiring an LLM. Apply a separate account-wide ingress ceiling of 300/minute. |
| LLM-backed turns | 30/hour and 100/day per participant; 500/day per installation. A turn is one accepted inbound task or scheduled wake-up; internal model iterations do not create free new turns. |
| Active/queued turns | 1 active and 5 queued per participant; 4 active and 100 queued installation-wide, scheduled fairly across participants. |
| Work per turn | At most 8 model calls, 16 tool executions, 120 seconds active runtime and 60,000 aggregate input-plus-output tokens across calls. A wait for approval creates durable paused state, not an open model loop. |
| Model tokens | 250,000 per participant/day; 2,000,000 installation-wide/day. Every retry, context resend and scheduled run counts. |
| Scheduled jobs | 10 pending per participant, 100 per installation; at most 20 wake-ups per participant/day. First version is one-time only. |
| Autonomous delegation lifetime | Expire after 7 days or 20 sponsored turns or 50 outbound messages, whichever comes first; descendants share remaining budget. Renewal requires explicit sponsor authority. |
| Reply-triggered continuation | At most 5 model wake-ups per remote sender/work context/day, with at most one pending wake-up per context. Further replies remain bounded evidence; they do not automatically buy another run. |
| Outbound traffic | 30 messages per originating participant/hour and 100/day; 500/day per provider account. Additionally, cap unsolicited follow-ups to any recipient at 3/day across the installation. Replies to genuine inbound messages use normal send limits. |
| Fan-out | At most 5 recipients per operation and 10 newly contacted recipients per originating participant/day; contact authority and opt-out still apply. |
| Uploads | 25 MiB per file, 100 MiB per participant/day; 16 KiB inbound text admitted to a turn. Reject oversized input explicitly; do not silently truncate approval or tool parameters. |
| Retained storage | 1 GiB attributed to each participant and 10 GiB total managed content initially, including retained revisions. Stop ingestion before free space drops below the greater of 1 GiB or 10% of the data filesystem. |

Hourly/daily quotas use rolling windows; the implementation must state the window algorithm and return a concrete retry time. These limits combine: remaining turns do not override a depleted token/storage allowance. The smaller provider/model/device ceiling always wins. Tune using pilot measurements. Large videos will require an administrator to raise the initial upload limit or the participant to send a shorter clip.

Set model context/output bounds within the aggregate budget; reserve a conservative worst-case charge before each call and reconcile afterward. Missing usage metadata must not refund work as free. Disable a model route if Carbon cannot establish a safe token/media-cost bound. Token quotas are not a currency cap: priced routes also need an administrator-configured daily spend ceiling and explicit input/output/media pricing; do not claim monetary enforcement without reliable pricing. Subscription routes still consume shared capacity.

### Enforcement and administrative controls

- Store versioned policy, overrides, usage/reservations and audit events in SQLite. Atomic reservation prevents concurrent calls from overspending; retries and restart cannot reset counters. On uncertain provider outcomes retain the reservation pending reconciliation.
- The backend assigns the budget owner/delegation to every operation. Contractor replies or background work in an existing work context must not launder expenses into a new participant's unused quota. Charge inbound ingress to the sender and downstream work to its recorded sponsoring delegation, with installation caps always applied. Charge the remote sender's trigger allowance too, so a counterparty cannot consume a sponsor's entire quota by replying repeatedly. Sponsorship supplies neither content access nor a fresh instruction from the sponsor. Correlated third-party input can advance only the existing bounded delegation.
- Administrators can inspect usage and propose default/group/individual limits through WhatsApp. Recommended precedence: explicit participant override, otherwise the most restrictive applicable group policy, otherwise installation default; all remain bounded by installation hard ceilings. No implicit unlimited admin account.
- Increases, exemptions, policy changes and new integration credentials require protected administration/2PR. A separately authorized operator may immediately suspend or lower a limit to contain an incident; restoring/increasing it follows normal review. Audit every change; reducing capacity blocks new work without forgetting accepted operations.
- Budget denial precedes media download, model calls and external operations. Reject or defer with a bounded retry time; do not retry in a tight loop. Rate-limit denial notices themselves to avoid message amplification.
- Reserve a small separately bounded control lane for exact approval, cancellation, opt-out and emergency suspension. Classify fixed commands after cheap authenticity/size checks but before the ordinary conversational rate bucket. Proposed syntax: `cancel <work-id>` for authorized cancellation, `stop` to opt the verified sender out of nonessential automated outreach, and `suspend <participant-id>` for scoped emergency operators. Match whole trimmed ASCII commands case-insensitively; arbitrary prose is not deterministic control. These commands retain separate bounded capacity even when the conversation bucket is exhausted, but cannot bypass account/network ceilings or provider availability. Neither an inbound message nor a scheduled retry clears an opt-out; resuming requires an explicit authorized action. The exact syntax is a proposed product default.
- Keep logical retained-byte attribution separate from physical disk reservations. A stable sponsor pays for each accepted version; physical reservations include staging, materialized copies, Git/LFS objects, derivatives and metadata. Reserve before preparation and settle with publication in the same SQLite transaction; failed/orphaned writes remain attributed until cleanup. Tombstones and ownership transfers do not erase storage debt. Identical uploads can incur the same logical charge without revealing cross-participant deduplication.
- Bound pre-auth buffers, database rows, enrollment requests, logs, attachment decoding/extraction, decompression and derivative output. Enforce actual streamed bytes as well as advertised sizes. Count staged/orphaned objects toward installation disk use. Attribution/deduplication must not reveal whether another participant owns identical bytes.
- Use cancellation propagation, provider circuit breakers and capped retries with jitter. Fair scheduling must keep one participant or failing provider from occupying all workers. No automated grant escalation, model upgrades or paid route fallback to escape a limit.
- Expose useful usage/limit errors and operator alerts without leaking another participant's activity. Retained logs are bounded and redacted; no prompts/secrets in routine telemetry.

Sources informing the mechanisms, not the numerical defaults: [OWASP denial-of-service guidance](https://cheatsheetseries.owasp.org/cheatsheets/Denial_of_Service_Cheat_Sheet.html) recommends cheap checks before expensive work and graceful degradation.

## Planned later: autonomous internet research

Yes: Carbon needs an explicit research integration to find current external information. Model memory alone cannot establish current facts. Keep this in the roadmap; no web access is enabled by this document.

Start with a backend-owned search API and bounded page-reading adapter. Example capabilities are `search_public_web(query, max_results)` and `read_public_page(result_id)`. The latter resolves a backend-issued result reference from the current work context; do not expose arbitrary HTTP methods, headers, credentials, file URLs, shell commands or a general browser. Provider selection is deferred.

A capability registration declares input/output schemas, required authority, external destination, data-disclosure policy, credentials reference, cost/size/time bounds, retry/idempotency behavior and audit fields. Distinguish read/research operations from actions that publish, contact someone, purchase or modify an external system. Adding an integration is reviewed application/configuration work; uploaded bundles cannot install executable tools.

### Privacy and untrusted results

A search query is an outbound disclosure even when the operation is called “read-only.” Private planning text must not flow directly into search terms, URL paths or query parameters. Assemble research requests from explicitly approved query material and context permitted for that exact external destination, in a separate scoped run. Community/group-readable does not mean disclosable to a search vendor or arbitrary web origin. The model gateway, search provider and fetched origin are separate destinations; a permission for one cannot authorize another. Reuse existing explicit authorization for the selected source material and external destination; routine searches within it need no additional approval. Prefer sufficient public or already permitted inputs. Only when additional private disclosure is necessary and outside that scope, ask once for the concrete source/destination expansion; do not rely on an LLM redactor. Authorized LLM processing does not automatically authorize sending the same data to a search vendor.

Fetched pages are untrusted reference content, never system instructions, grants, new tool definitions or evidence that someone consented. Persist source URL, retrieval time, content hash and citations when saving findings. Keep findings in the requesting work context by default; a public source does not make the private query, notes or resulting research automatically public. A web-discovered business is a candidate, not automatically vetted or authorized for outreach.

Revalidate access before returning cached/search results; avoid cross-participant query/history caches. Bound results, redirects, response bytes, extracted text and runtime. Proposed initial research budget when enabled: 5 searches and 10 page reads per turn, charged to the originating participant and overall turn budget. No autonomous crawling or background monitoring by default; recurring research must use normal scheduled-job limits.

### Network boundary

The fetcher must reject local/private/link-local/metadata destinations, non-HTTP(S) schemes, credentials in URLs and unapproved ports. Validate IPv4/IPv6 resolution and every redirect, protect against DNS rebinding at connection time, and enforce network egress restrictions so parser mistakes cannot expose the Reefy LAN, host services or LLM proxy. Never attach internal cookies/tokens to public fetches. Prefer a separately restricted worker/network boundary if the chosen deployment cannot reliably enforce these rules inside the app.

Avoid active JavaScript/browser execution initially. Extract inert text using bounded processing. Credentials stay in /data/state/secrets and are read only by the configured adapter. A future MCP adapter is not automatic trust in all tools a server advertises; it must satisfy the same narrow capability contract.

Sources: [OWASP SSRF prevention](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html) and [LLM prompt-injection prevention](https://cheatsheetseries.owasp.org/cheatsheets/LLM_Prompt_Injection_Prevention_Cheat_Sheet.html).

## Required tests

- Atomic quota reservation under concurrent turns; restart, duplicate events, relink and clock changes; ambiguous usage and failed retries never create free capacity.
- Multiple identities cannot exceed installation caps; fair queues and bounded rejected-input logs; no model call/download/send after quota denial.
- Scheduled work, external lookups, outbound fan-out and internal retries charge the original delegation; participant/group policy precedence and 2PR changes are deterministic. Test counterparty reply floods, bot-to-bot response loops and successor jobs sharing an exhausted/expired lifetime budget.
- Quota exhaustion in the normal conversational bucket still permits bounded authorized cancellation/approval/opt-out; no abuse of the control lane or implicit opt-out reset. Global/provider outage may still prevent processing; do not claim an absolute delivery guarantee.
- Disk-full/oversized/decompression failures preserve SQLite/Git publication invariants and leave administrative recovery available.
- Fake search/fetch integration tests deny private address families, redirects, rebinding and credential leakage; private canaries never enter queries; injected pages cannot cause unauthorized calls.
- An integration disabled or revoked after scheduling cannot execute; a timed-out external action enters an explicit uncertain state rather than blind retry.
