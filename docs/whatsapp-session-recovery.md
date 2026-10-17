# WhatsApp session recovery

A terminal WhatsApp `LoggedOut` event differs from a temporary transport
disconnect. The library disables automatic reconnect on terminal logout but
does not erase its persisted device keys. A saved companion identity therefore
does not establish that WhatsApp still accepts that link.

Carbon previously cleared only in-memory state, retained the old protocol store,
and hid the unlink action whenever no client was connected. The unlink API
also refused that state. Opening the pairing panel could not produce a fresh
QR code: the old credentials were reused instead. The UI nevertheless displayed
"Preparing QR code" indefinitely.

Carbon now persists the terminal reason and `on_connect` flag, pauses pairing until
the operator chooses **Relink WhatsApp**, and offers that action without a
connected client. Relink uses the existing authenticated, same-origin reset
path: cancel pending work, rotate the session namespace, stop the old bot,
remove the old protocol store, and prepare fresh pairing on demand. Saved
conversations, files, participant permissions and configuration are preserved.
No logout callback automatically deletes credentials. Account lock events are
also terminal and never retried automatically. Ordinary disconnect/reconnect
continues using the original store.

Logs and status include only the classified reason and connection flag. Never
log raw protocol stanzas, QR payloads, device keys, phone numbers, or appeal
tokens. Generation checks ignore callbacks from a replaced transport.

## Regression coverage

- Real temporary protocol SQLite store with a saved companion identity, then
  terminal logout: visible pairing cannot reopen it; keys remain until reset.
- Restart retains the terminal state; explicit reset produces a new store and
  removes the old one while preserving application settings and existing data.
- Account-lock classification remains distinct from ordinary revocation.
- Old transport callbacks cannot revoke the replacement session.
- API relink works without a client, still requires authentication and mutation
  header, and rejects phone-code requests before reset.
- Browser state shows a relink action and explanation instead of a permanent
  QR spinner; transient disconnects do not offer destructive reset.

External acceptance still requires a disposable WhatsApp account to scan a
fresh QR, verify reconnection after restart, unlink from the phone, then relink.
Unit/API/browser fixtures do not prove live provider compatibility.


## Existing feature configuration and upgrades

The release includes the existing optional `experimental_tasks`, `debug_trace`, and bounded `model_concurrency` implementation. Upgrades retain these settings and their functionality; configuration files are not rewritten or stripped to permit startup. Defaults remain tasks/tracing disabled and model concurrency four. Unknown fields still fail validation.

`tests/config_upgrade.py IMAGE` starts and recreates an offline disposable container with synthetic task/tracing settings, verifies health and exact configuration preservation, and never contacts WhatsApp. The normal container-check workflow runs it alongside application tests, authentication/persistence smoke checks, and desktop/mobile navigation E2E. The browser regression includes the terminal-logout view and explicit relink action.
