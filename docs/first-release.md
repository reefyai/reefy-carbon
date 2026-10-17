# First runnable release: v2026.09.26-01

This development release implements a private-conversation slice of the accepted architecture. It does not yet implement autonomous multi-party coordination.

## Available

- Native WhatsApp linked-device connection using `oxidezap/whatsapp-rust` 0.7.0, with expiring QR and phone-number pairing codes protected by the setup access policy. No Node bridge, public webhook or model shell access.
- OpenAI-compatible model discovery and bounded chat/tool calls through `OPENAI_BASE_URL` (Reefy injects its LLM proxy endpoint) and `OPENAI_API_KEY`.
- Stable UUID participants bound to actual incoming direct-message identities. A WhatsApp LID remains a provider identity; it is not treated as a phone number. Unknown identities are separate participants. Aliases and phone changes are not automatically merged.
- Private conversation history and note tools: `list_notes`, `read_note`, `save_note`. Rust supplies the actor; the model cannot choose an owner, filesystem path or recipient. No permission prompts during private conversation.
- SQLite authority, request deduplication, durable inbox/outbox and request budgets. In-flight model work becomes interrupted after restart. Ambiguous sends become unknown and are not automatically resent.
- Immutable notes under `/data/content/resources/<resource UUID>/content.md`, with local Git history and retained commit refs. Notes above 4 KiB use generated Git LFS pointers and SHA-256-addressed local objects. SQLite records the representation; user-authored pointer text is never followed. No Git/LFS server is needed. The app reads the authoritative object, not the materialized working copy. Do not edit this repository externally or run pruning.
- One-time protected setup can seal two verified, distinct initial admin identities. From v2026.09.26-08, these reviewers decide durable group/access proposals through exact WhatsApp commands. See [permission pilot](permissions-pilot.md).

## Setup

Install the Carbon Reefy app and open its service. Default installations require `admin` and the generated password at `/data/state/secrets/admin-password`; deployments configured for optional authentication can rely on Reefy login (see below). Credentials are never logged. Scan the QR using WhatsApp's **Linked devices** screen, or select **Get linking code** after entering your international phone number. Configure a provider in Reefy, refresh models and save a model. Send a private text message to the linked account from a different account.

Private text messages from any incoming direct identity are accepted subject to the limits below. Use a dedicated test account for the pilot. WhatsApp group messages, outgoing echoes, offline/history sync and disappearing/view-once messages are not processed. Ordinary media uploads are accepted from v2026.09.26-09. Messages arriving before a model is selected remain queued. The QR, pairing-code endpoint and setup page share the configured access policy.

Initial admins must each send an incoming text first; select their verified transport identities and seal the selection once. This does not give the setup UI access to message history or notes. Later admin replacement awaits a protected recovery flow; do not edit SQLite to work around it.

## Persistent layout

```text
/data/
  state/
    config.json
    secrets/admin-password
    carbon.sqlite                 # identities, ACLs, reviews, messages, queues, quotas
    carbon.sqlite-wal             # when active; back up the entire volume
    whatsapp/session.sqlite   # linked-device protocol state and keys
    instance.lock
  content/
    .git/                     # local resource history and retained refs
      lfs/objects/            # actual LFS payload bytes
    resources/<uuid>/content.md
```

All of `/data` is included in Reefy's backup volume. Source, libraries and compiled artifacts are only in the image. Legacy `/data/config.json` is moved into `/data/state/config.json` without modifying its referenced secret path, preserving existing passwords. Conflicting old/new configs fail startup. An old referenced `/data/secrets` directory remains backed up; do not delete it until its config reference is migrated.

## Configuration and bounds

`/data/state/config.json` accepts:

```json
{
  "listen": "0.0.0.0:18080",
  "auth": {"mode": "basic", "password_file": "/data/state/secrets/admin-password"},
  "daily_user_limit": 50,
  "daily_global_limit": 500
}
```

Limits count accepted requests per UTC day and survive restarts. Administrators can change protected configuration and restart the app. Other fixed pilot limits: 1,000 participants, 100 notes per participant, 16 KiB per note, 8 KiB incoming text, 16 recent messages, 256 KiB model context, eight model rounds (the last one answers without tools), twelve tools per round (extra calls are skipped with an error the model can retry), 800 output tokens per call. Requests over limits are rejected; a friendly rate-limit response is a later slice; sealed reviewers have a reserved approval-command lane. Provider failures receive a generic reply without exposing credentials or gateway details.

## Tests and remaining gates

`cargo test --locked` covers authentication, mutation-header enforcement, actor isolation, forged tools, actual HTTP model/tool execution with a deterministic fake gateway, quotas, replay deduplication, sealed bootstrap, LFS pointer injection, corrupted payloads and restart handling. `cargo clippy --locked --all-targets -- -D warnings` checks the whole crate. `python3 tests/container_smoke.py IMAGE` exercises the real Linux image, non-root/read-only startup, authentication, graceful stop and persisted credentials.

Not implemented: write/version permissions, generic coordination scopes and cross-recipient provenance propagation, live bundles, video understanding, additional document/audio formats and cross-recipient attachment delivery, semantic search, scheduling, reviewer replacement, phone relinking, internet integrations, operator reconciliation of interrupted/unknown work, retention/compaction and cross-host/restore activation fencing. The process lock prevents concurrent use of the same volume; it does not fence a copied volume on another host. Restore testing must keep the restored app disconnected until reviewed; production multi-tenant rollout remains blocked on the accepted restore/authority contract. Do not interpret passing pilot tests as coverage of unimplemented mechanisms.

The accepted architecture and zero-permission-prompt coordination behavior remain the target. This slice contains no business-specific entities or workflow logic.

## Optional admin password (v2026.09.26-02)

For a Reefy installation already protected by Reefy's authentication proxy, use `"auth": {"mode": "optional_basic", "password_file": "/data/state/secrets/admin-password"}`. This follows CamAdmiral: an existing, nonempty password file enables HTTP Basic; a missing file disables the additional app password. Empty or unreadable files fail startup. Carbon reads this setting at startup, so restart after changing it. Rename the old password file to retain it for reversal. The entrypoint preserves existing configuration and does not regenerate a removed password while that configuration exists.

In this mode, access control is provided by Reefy rather than a separate Carbon login. Keep the app's published port on host loopback behind the Reefy proxy. The setup UI has operator authority; participant identities and 2PR approval remain separate. Mutation-header checks remain enforced. Standalone installations still default to required Basic auth; the existing `disabled` mode still requires a loopback listener.

## Phone-number linking (v2026.09.26-03)

Open Carbon on the same phone as WhatsApp, enter the account phone number including its country code, and select **Get linking code**. Switch to WhatsApp → Linked devices → Link a device → Link with phone number instead, then enter the displayed code. QR linking remains available.

Codes expire after approximately three minutes, stay in memory only, and disappear after connection. Repeating a request for the same number returns the existing valid code; another number must wait for expiry. Requests are serialized and failed attempts have a minimum one-minute cooldown (longer if WhatsApp requests it). The endpoint uses the same authentication and mutation protection as other setup actions. It cannot relink an already connected account. No phone number or code is logged by Carbon.

## Unlink WhatsApp (v2026.09.26-04)

Connected accounts show **Unlink WhatsApp**. Confirming stops incoming work, lets an already active reply finish, cancels remaining queued work, requests companion-device deregistration, removes the local protocol session, and opens a fresh pairing session. Conversations, files, model choice and initial admins remain. Session rotation is committed before logout so a restart cannot reuse the old session. New sessions live at `/data/state/whatsapp-<uuid>/session.sqlite`; the original installation used `/data/state/whatsapp/session.sqlite`. Both are inside the backed-up data volume.

Remote deregistration is best-effort in the library, particularly when offline; remove Carbon from WhatsApp's Linked devices screen too if it remains listed. Existing backups may retain old session files; the restore fencing requirement still applies.

## Automatic QR renewal (v2026.09.26-05)

The first QR in each library-provided batch lasts about 60 seconds; subsequent codes last about 20 seconds. The UI shows remaining time and updates only when the code changes. After a batch is exhausted, Carbon renews the unpaired connection automatically, with a five-second grace period and at least 30 seconds between recovery attempts. Existing session storage and saved data are preserved. Renewal is suppressed during unlink, after login, and while a phone pairing code remains valid.

## Activity and debug log (v2026.09.26-06)

The collapsed **Activity & debug** panel shows the newest 100 operation events and refreshes every five seconds while open. It records startup/shutdown, WhatsApp connection events, QR renewal, phone-code requests, unlink, model selection, reviewer setup, incoming private text activity, model work and outbound delivery outcomes. Chat events include the WhatsApp identity reported by the transport (phone JID or opaque LID); Carbon does not invent a contact name or resolve an opaque ID to a phone number.

Events are stored in `activity` in `/data/state/carbon.sqlite`, survive restart and are included in the existing backup. Retention is bounded to 1,000 events and seven days (pruned on writes; older entries are excluded on reads). They are also emitted as structured JSON to container stderr, whose retention is managed by the host. The separate permissions audit is unchanged. Access uses the same admin setup policy and no-store headers as other management endpoints. Events contain fixed application messages, not chat contents, prompts, model replies, raw provider errors, tool arguments, QR payloads, pairing codes or credentials. Activity starts with this release; earlier operations are not reconstructed.

Each LLM round trip has request/response events, and successful private-note tools record save/read/list events. Failed or disallowed tools are recorded without arguments. Incoming document/media messages are explicitly marked unsupported; they are not falsely logged as uploaded or shared. File ingestion and sharing remain planned features and should emit success events only after those operations are actually committed.

## Collapsed setup sections and on-demand pairing (v2026.09.26-07)

All sections start collapsed. Only expanding the WhatsApp section starts an unlinked transport and QR rotation. Visible panels renew a per-tab 30-second lease; closing a panel removes its lease, and closed/crashed tabs expire automatically. QR work stops when no visible viewers remain. Linked accounts reconnect and process messages independently of the UI. A valid phone-number pairing flow keeps its connection until completion or expiry so switching to WhatsApp does not invalidate the code; QR display is suppressed while the panel is hidden. Model and participant lists load on expansion.

See [implementation status and remaining milestones](implementation-status.md) for the code-verified feature inventory.

## Shared text and reviewed administration (v2026.09.26-08)

Read grants, groups and executable 2PR are available through WhatsApp. See [the exact shipped scope and examples](permissions-pilot.md). The UI shows a rotating status indicator while preparing or renewing a QR code, only while the WhatsApp panel is open and visible. Reduced-motion preferences disable the rotation.

## Attachments and inherited folders (v2026.09.26-09)

WhatsApp files up to 8 MiB are stored privately in Git/LFS, including before model selection. Small UTF-8 text attachments can be read. Logical folders inherit read grants; same-owner moves require 2PR. See [behavior, bounds and remaining limitations](attachments-folders.md).

## Photo understanding (v2026.09.26-10)

`analyze_image` reads authorized immutable JPEG/PNG/WebP content and sends it through the existing proxy to the selected model. Vision support is required; failed/unsupported requests produce tool errors, never fabricated success. No new permissions prompt is introduced. See [attachments and folders](attachments-folders.md) for boundaries and tests.

## File replies (v2026.09.26-11)

Participants can ask Carbon to send back an accessible stored file or note. Delivery is bound to that participant, permission-checked before upload and dispatch, deduplicated and bounded by durable quotas. Failed model turns do not release staged files; uncertain sends are not replayed. No additional permission prompt or model selector is introduced. See [attachments and folders](attachments-folders.md).

## Provider attachment understanding (v2026.09.26-13)

The local PDF worker from v12 is removed. `analyze_document` uses xAI Files/Responses for text and scanned PDFs; `transcribe_audio` uses xAI speech-to-text for Ogg/Opus, WAV, MP3 and FLAC. Existing image analysis remains provider-based. No local format decoder/OCR/transcription engine or new model selector is introduced. Rust retains permission, integrity, quota and lifecycle checks. See [attachments and folders](attachments-folders.md) for supported-provider constraints, temporary upload cleanup and tests.

## Search and review discovery (v2026.09.26-14)

`search_resources(query)` finds accessible notes, resource names and bounded UTF-8 file contents through SQLite FTS5. Results recheck current inherited and source-dependent access and record only returned resources as context sources. `pending_reviews()` discovers visible active requests without requiring participants to know UUIDs. Exact approval remains a human request-bound command handled by Rust. See [search and reviews](search-and-reviews.md).

## WhatsApp device name (v2026.09.26-15)

Carbon supplies `Carbon` as its WhatsApp companion OS/device label instead of the library default `rust`. This applies to new pairing metadata; WhatsApp may retain the name of an existing link until it is unlinked and paired again. The upgrade preserves the existing session and does not automatically unlink it.

## Grouped operations (v2026.09.26-16)

Debug activity now groups message processing and delivery by a persistent UUID, with collapsed summaries and chronological details. Existing events remain visible without guessed correlation. See [web UI](web-ui.md#grouped-debug-activity-v20260926-16).

## Admin setup and identity labels (v2026.09.26-17)

Setup accepts one or more verified admins with a dynamic “+ Add admin” control. Any admin can explicitly approve any valid request, including their own. The UI calls this Admin approval; independent second-person review is no longer required. Phone labels use native WhatsApp mappings and retain the LID in parentheses. See [current policy](permissions-pilot.md#current-approval-policy-v20260926-17).
