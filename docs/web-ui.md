# Current release

See [first runnable release](first-release.md) for current setup and storage paths. The notes below describe the original setup shell and its design history.

# Web UI starter

A responsive, backend-served operator page inspired by CamAdmiral. Rust/Axum embeds the HTML, CSS and JavaScript into one executable. There is no frontend build system or runtime Node dependency.

Implemented: a minimal setup-page shell for WhatsApp linking, LLM proxy model selection and true-admin selection, optional HTTP Basic authentication, per-peer failed-login throttling and strict JSON configuration. Setup controls are disabled until their backend integrations exist; the UI has no marketing copy or service-status dashboard. This is a read-only starter, not the full coordinator. No WhatsApp account, LLM, SQLite application state, pairing QR, 2PR executor or permission editor is wired in yet.

For Docker/Compose and Reefy packaging, see [deployment](deployment.md).

Current starter packaging uses `/data/config.json` and `/data/secrets/admin-password`. The coordinator target is `/data/state/config.json` and `/data/state/secrets/admin-password`; migration is pending and must retain existing credentials. See [target content/state layout](content-storage.md#layout).

## Run

Use a current stable Rust toolchain (validated with 1.98.1).

1. Copy `carbon-web.example.json` to `carbon-web.json`.
2. Set `auth.password_file` to a protected UTF-8 file containing a strong password (16–1024 bytes; trailing line endings removed). Do not commit that file.
3. Run `cargo run --locked -- carbon-web.json`.
4. Open `http://127.0.0.1:18080` and use username `admin` with that password.

The sample uses a secret path within Carbon’s backed-up data volume. For local development, point it at your own protected file. Auth is optional only by explicitly selecting:

```json
{
  "listen": "127.0.0.1:18080",
  "auth": { "mode": "disabled" }
}
```

Disabled mode rejects non-loopback listeners. Missing/empty/invalid configured password files fail startup. There is no default password or auto-detection that silently disables auth. Secrets/config are loaded on startup; restart to rotate the credential.

Use HTTPS at the Reefy/reverse-proxy boundary for network access: Basic credentials are not encrypted by HTTP. Restrict backend reachability to the proxy. Do not publish the management UI through a public webhook route. A loopback backend can still become remotely reachable through a proxy; disabled mode is not a substitute for proxy authentication.

The current throttle uses the direct peer address, ignores untrusted forwarded headers and holds at most 4,096 failure entries. Five failures lock that peer out until its five-minute window expires; behind a proxy, users may share a bucket. Trusted-proxy identity support is future work.

## API and boundaries

- `GET /`, `GET /app.css`, `GET /app.js`, `GET /api/status`: protected when Basic auth is configured.
- `GET /healthz`: public minimal liveness only.
- All responses use no-store; the page permits same-origin assets only and disallows framing.
- No mutation routes exist. Later mutations require CSRF/origin validation, actor binding and normal Carbon authorization.
- Shared web credentials confer operator status, not a participant identity or a 2PR approval. Do not add an approve button until an individual reviewer authentication flow is designed.
- Do not expose participant histories by virtue of knowing this shared password.

## UI style

Use **Carbon**. Show only useful controls, labels, errors and required setup guidance. No extra branding, slogans, development badges, generic service/uptime panels or architectural explanations.

## Next UI increments

1. Authenticated WhatsApp connection status and short-lived QR display once the selected native whatsapp-rust adapter is integrated; do not cache or log QR/session secrets.
2. LLM gateway status with secrets hidden.
3. Scoped pending 2PR summaries and audit metadata, with review still happening through identified WhatsApp participants.
4. Redacted job/transport diagnostics and safe settings operations backed by the existing authorization layer.

Use actual backend states; do not invent paired accounts, approval records or operational counters for the dashboard.

## Verification

`cargo test --locked` checks route protection, authenticated status/no-secret output, lockout and fail-closed configuration. `cargo clippy --locked --all-targets -- -D warnings` checks the Rust starter. Integration/device tests for WhatsApp are a separate pending milestone.

## Grouped debug activity (v2026.09.26-16)

The debug panel groups each admitted incoming message into a collapsed operation. Intake stores a UUID atomically with its inbox row. LLM requests/responses, tool results, attachment processing and queued text/file delivery carry that UUID. Delivery resolves it from the durable inbox relationship; process restart and duplicate messages retain the same ID, while overlapping messages from the same participant receive distinct IDs. Structured stderr logs and authenticated `/api/activity` events include `operation_id`.

Each row shows the time, participant identity, latest logged step, elapsed time between retained events and error count. Expand it to see the operation UUID and chronological events. Open rows stay open across polling; new rows start collapsed. Event descriptions are rendered as text, never HTML. Durations use second-resolution timestamps and describe the retained event span; a `≥` prefix indicates the intake event is absent. An error remains visible in the summary even if a later fallback reply is sent.

The endpoint returns up to the existing 1,000-event / seven-day retention bound so a 100-event response cutoff does not unnecessarily split groups. Older events without correlation remain standalone; Carbon does not guess historical groups from identity or timestamps. Pairing, connection, admission failures without an inbox row, and independent review notifications remain standalone system/event entries. Grouping is diagnostic, not the authoritative delivery-state or audit history. No message bodies, model prompts/replies, attachment contents or secrets are added. Future multi-party workflows can connect several operations; no workflow grouping is inferred in this release.

Tests cover operation IDs across restart and durable outbox delivery, overlapping same-participant requests, deduplication, legacy schema migration, chronological rendering, collapsed defaults and expansion persistence.

## Admin approval setup (v2026.09.26-17)

Start with one admin selector; use “+ Add admin” and Remove for additional slots before sealing the initial set. Empty slots are ignored; zero, duplicate and unverified selections are rejected by Rust. Any configured admin can explicitly approve requests including their own. Reviewer membership remains one-time setup; this does not add post-bootstrap reviewer replacement. Known WhatsApp phone mappings are displayed as `+phone (@lid)` in selectors and operation summaries; no identity or permission assignment changes when a display mapping changes. Refresh participants remains available after setup to refresh labels.

## Admin approval requests (v2026.09.26-18)

A separate collapsed **Admin approval requests** panel shows pending and recent requests, canonical summary, requester phone/LID where known, status and expiry. It fetches the latest 100 through protected `GET /api/approval-requests`, refreshing every five seconds only while open and visible. Expired pending records are displayed as expired even while WhatsApp is offline. Text is rendered with `textContent`. Approval stays in authenticated WhatsApp replies; the shared web operator session has no approval buttons or private-resource read override. No long request UUIDs or operation JSON are shown.

## Current admins (v2026.09.26-20)

After enrollment, the admin selectors display the current approval-admin set and remain disabled. Status refresh detects approved additions/removals, refreshes participant labels while the panel is open, and shows guidance to request changes through WhatsApp. Missing labels display “Participant not loaded” rather than a raw UUID. No web approval or post-bootstrap role-mutation route is added.
