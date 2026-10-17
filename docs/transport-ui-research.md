# Rust WhatsApp transport and web UI research

Reviewed 2026-09-26. Repository inspection, not a live WhatsApp/device validation.

## Selected initial transport: native Rust

[oxidezap/whatsapp-rust](https://github.com/oxidezap/whatsapp-rust) is the strongest candidate found for Carbon's native linked-device adapter. It implements the WhatsApp Web protocol, not the official Cloud API. Its documented features include QR/pair-code linking, persistent sessions, text, quoted replies, media, groups and Tokio/SQLite backends. The MIT-licensed [v0.7.0 release](https://github.com/oxidezap/whatsapp-rust/releases/tag/v0.7.0) was published 2026-08-07. Its release notes report over a year of development and multiple downstream users.

Evidence beyond the feature list:
- The release has explicit breaking-change migration notes: active development, but a pre-1.0 integration maintenance cost.
- [E2E CI](https://github.com/oxidezap/whatsapp-rust/blob/v0.7.0/.github/workflows/e2e.yml) exercises a mock WhatsApp server. That server image is private and fork PRs cannot run that job without access; this is not a public live-account compatibility guarantee.
- [Durability CI](https://github.com/oxidezap/whatsapp-rust/blob/v0.7.0/.github/workflows/signal-durability-nightly.yml) includes deterministic Signal-state chaos tests and an abrupt SQLite process-restart test.
- The current issue tracker includes [store release lifecycle](https://github.com/oxidezap/whatsapp-rust/issues/1535), [reconnect admission](https://github.com/oxidezap/whatsapp-rust/issues/1532), and [reply context helpers](https://github.com/oxidezap/whatsapp-rust/issues/1533). These are review targets, not evidence that ordinary messaging is broken.
- The v0.7.0 Cargo manifest specifies Rust 1.94 minimum. Pin and test an exact version/revision with a lockfile; do not assume README examples on main match a release.

**Decision (2026-09-26): adopt whatsapp-rust directly for the first Carbon adapter, using the reviewed v0.7.0 release as the initial integration baseline.** Pin the dependency/lockfile; review newer releases before upgrading. It is credible enough for a pilot, with production readiness contingent on the acceptance checks below. Do not build a Node bridge in parallel. Keep Baileys as a contingency for a blocking gap and Cloud API as a future official option. Rust does not remove the account-restriction or compatibility risks of an unofficial client. The adapter has not been implemented yet.

Pilot acceptance: QR pairing; restart without re-pairing; DM text and image/PDF/video exchange; sender LID/phone mapping; reply-context integrity for 2PR; reject outgoing echoes/history/forwards as approval; reconnect after network loss; deduplication and ambiguous-send reconciliation; session revocation; prolonged idle/reconnect operation; CPU/RAM measurement on the target Reefy device. No account has been linked or these tests run during this research.

## Other candidates

- [WhatsRust](https://github.com/199-biotechnologies/whatsrust) is a larger Rust daemon/library built on whatsapp-rust; its README identifies v0.5.0 and advertises REST, CLI, MCP and its own outbound queue. It is not an independent protocol stack. Carbon already owns queues and narrow tools, so direct library integration is a smaller fit. Its advertised memory/size and “anti-ban” claims were not independently verified and are not guarantees.
- [baileyrs](https://github.com/oxidezap/baileyrs) wraps Rust/WASM behind a Baileys-compatible JavaScript API and requires Node. It does not meet the objective of eliminating the JS runtime.
- [wa-agent](https://docs.rs/crate/wa-agent/latest) also exists; no stronger maturity evidence was established in this review. Do not choose solely because it is Rust.

## CamAdmiral learnings

Inspected [README](https://github.com/reefyai/reefy-camadmiral/blob/main/README.md), [auth.py](https://github.com/reefyai/reefy-camadmiral/blob/main/camadmiral/auth.py), [config.py](https://github.com/reefyai/reefy-camadmiral/blob/main/camadmiral/config.py), [app.py](https://github.com/reefyai/reefy-camadmiral/blob/main/camadmiral/app.py), and [index.html](https://github.com/reefyai/reefy-camadmiral/blob/main/camadmiral/index.html).

Useful patterns: responsive static HTML/CSS/JavaScript with no frontend build runtime; backend-served assets; HTTP Basic username admin with a password file; constant-time credential comparison; failed-login throttling; validated versioned configuration; external secret mounts; persistent-volume boundaries; HTTPS termination at a reverse proxy.

Adapt rather than copy:
- CamAdmiral permits auth when its password loader returns None. Carbon requires an explicit disabled mode; missing/empty configured secrets are fatal.
- Disabled auth is loopback-only in the starter. A future Reefy-proxy mode must be explicitly configured with network restrictions, not inferred from an arbitrary forwarded header.
- Protect all UI and management API routes. Keep only minimal liveness public. A future signed WhatsApp webhook is a separate route/auth policy.
- Shared Basic credentials identify a web operator, not a distinct participant. They cannot satisfy 2PR or silently grant access to personal histories.
- Any later mutation endpoints need same-origin/CSRF checks as well as backend authorization. Basic auth alone does not prevent CSRF.
- Never expose QR/session credentials, API tokens or private content through public health/status responses.

See [web UI starter](web-ui.md) for the implemented, deliberately small surface.

