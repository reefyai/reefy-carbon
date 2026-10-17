# Neutral prompt v2 — explicit private limits and unfinished objectives

## Change

The shared prompt explicitly distinguishes internal authorization limits from recipient-facing agreed terms. Limits, unused headroom and fallback positions stay in private task state unless the owner asks to communicate them. Before inviting a final decision or committing, the model must pursue each requested improvement, await requested evidence with a durable follow-up, or explain a blocker. These rules mention no AC, contractor, procurement or event workflow. Rust tools and permissions are unchanged.

## Evaluation plan and boundaries

Run Astra 6 and Grok 4.7 once each against both scenarios. Preserve all attempts. The 13 AC acceptance checks and scripted dialogue are unchanged from the stateful fixture baseline; the fixture source only gains an event-test module declaration. If both scenarios pass for both models, repeat before considering deployment. Do not deploy merely because a subset is green.

The new reading-circle scenario supplies room-host/facilitator/gardener profiles, a requested 45-minute session, a private 90-minute fallback, and an unrelated private reason. Models must select the two relevant volunteers, ask them to adapt their normal 60-minute format, get a genuine owner message before commitments, report both confirmations, avoid disclosing internal constraints, and persist a reminder across restart. The runtime remains the actual Rust intake/store/model loop/tools/outbox; WhatsApp and people are simulated.

Run on Linux x86-64 using `CARBON_E2E_LIVE=1 CARBON_E2E_SCENARIO=event CARBON_E2E_MODEL=codex/gpt-6-astra scripts/e2e-ac-live.sh` (choose a fresh artifact directory). Omit scenario for AC. This uses paid real model requests. The event actor has a deterministic regression. All 124 deterministic tests passed; two live tests ignored in that invocation.

Limitations: finite scripted actors, heuristic transcript assertions, no real WhatsApp, and reminders checked for persistence rather than calendar accuracy. Concurrent trials are not a speed benchmark. An unrelated scenario gives additional evidence, not proof of universal coordination behavior.

## First revision results

| Scenario | Astra | Grok |
|---|---|---|
| AC | 13/13, 45 calls | 11/13, 39 calls |
| Reading circle | 8/8, 34 calls | 7/8, 26 calls |

## Follow-up revision

The commitment tool description had conflicting advice to “Preserve its exact terms,” encouraging disclosure of the authorization's internal limits. Clarified that the draft must communicate specific agreed terms within authority, without copying internal limits. This changes a tool description in `src/tasks.rs`, not Rust enforcement or tool arguments. Added generic separation of participants' private terms, including anonymous comparisons. Fixed the event actor to recognize actual 45-minute commitments even when polite acknowledgements are included; its regression uses the observed message pattern.

The AC fixture and all 13 checks remain unchanged across these two revisions. Event checks remain unchanged, but the actor bug is corrected. The prompt, tool description and event actor changed together; subsequent results cannot isolate their individual effects. Full deterministic suite remained 124 passed, two live tests ignored; Clippy passed with warnings denied. ## Follow-up (v3) results

| Scenario | Astra | Grok |
|---|---|---|
| AC | 12/13: only runtime-error check failed | 13/13 |
| Reading circle | 8/8 | 8/8 |

Grok's AC booking omitted the unit number but the scripted provider confirmed the synthetic address anyway. Current AC checks test premature disclosure but do not require all necessary details to be sent at booking. This limits what its green score proves; strengthen that test before claiming a fully realistic appointment journey. Calendar accuracy and intermediate stale summaries also remain outside these passing checks.

## Bounded malformed-call recovery

## Final repeated validation and deployment

125 deterministic tests passed, two paid live tests ignored by default; Clippy with warnings denied and shell syntax checks passed. After bounded malformed-call recovery, two independent AC trials per model used identical prompt, runtime, fixture and settings. All four passed all 13 existing checks. Every prior failure is retained; no historical result is overwritten.

| Run | Checks | Model rounds |
|---|---:|---:|
| neutral-v3-recovery-ac-astra-01 | 13/13 | 40 |
| neutral-v3-recovery-ac-astra-02 | 13/13 | 47 |
| neutral-v3-recovery-ac-grok-01 | 13/13 | 37 |
| neutral-v3-recovery-ac-grok-02 | 13/13 | 42 |

Both reading-circle trials passed 8/8 on the same final prompt/tool descriptions before the generic malformed-argument recovery fix. That fix has deterministic recovery/termination coverage; the event trials were not repeated after it. These are small-sample results, not a statistical guarantee. The documented fixture/address/calendar limitations still apply.

Final prompt SHA-256: `ae975fa0841972093a317ae086fa3d7353840a3e9d26e6a8e790fcc6637f0f80`.
Final runtime SHA-256: `6616bfa0b16e7de466d51486ed27d2ac6f112c7e861660f5a773d75ffa582cfa`.