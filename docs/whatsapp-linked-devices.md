# WhatsApp linked-device message intake

## Defect and fix

`whatsapp-rust 0.7.0` parses an incoming private stanza with `chat = from.to_non_ad()` and `sender = from.clone()`. A linked-device sender such as `100000000000001:7@lid` therefore belongs to chat `100000000000001@lid`. Carbon previously compared the two strings and returned before ingestion or activity logging. Primary-device messages without a suffix passed; linked-device messages could disappear silently.

The intake boundary now compares typed JIDs after `to_non_ad()` normalization. It retains the canonical chat identity for storage, quotas and authorization. It does not merge phone-number and LID identities or admit a genuinely different sender. Unlinking, self-sent, group/broadcast, offline, disappearing and unsupported-address exclusions remain in effect.

The live callback delegates to `Runtime::receive_message`; the automated application journey exercises that same method. No test-only ingestion HTTP endpoint is exposed.

## Diagnostics

Activity & debug and container stderr record fixed, content-free intake events:

- `whatsapp_linked_device`: normalized accounts match, but the original sender/chat comparison would have rejected this message.
- `whatsapp_direct_message`: direct sender already matches its chat.
- `whatsapp_message_ignored`: a fixed admission-rejection reason; no rejected sender or content is recorded.

Accepted diagnostics use the canonical account identity already used by activity logging. No message bodies, device-specific identifiers, raw stanzas or encryption material are added to logs. Existing retention bounds remain 1,000 events / seven days. Intake events occur before an inbox operation is created; `message_received` and subsequent events retain their operation IDs.

## Automated checks

`cargo test --locked` runs:

- Parser-based unit regressions for primary and linked devices 1, 7 and 255, using both PN and LID addresses. The fixture also demonstrates that the old comparison fails for linked devices.
- Negative admission cases for genuinely different accounts, PN/LID mismatch, own/group/broadcast/unsupported/offline/disappearing messages and unlinking.
- Application E2E in `tests/app/whatsapp_ingress_e2e.rs`: actual WhatsApp metadata parser → production receive handler → SQLite inbox → authenticated HTTP Activity API → durable reply outbox; duplicate delivery across devices and process-store reopen retain one participant and one job per message. Private canaries never enter activity output.

The E2E uses synthetic decoded stanzas and a deterministic response. It does **not** simulate WhatsApp's network, Signal decryption or successful delivery to a real phone; live acceptance is separate.

## Live acceptance

Send `Carbon-IPHONE-CHECK-2` and `Carbon-MAC-CHECK-2` to the same Carbon chat, ideally several seconds apart. After both are processed, run `tests/whatsapp_live_check.py` with the Carbon SQLite file mounted read-only and `--round 2 --expect-both`. Confirm each marker has one inbox row, `message_received` activity and reply delivery; the Mac intake should include `whatsapp_linked_device`. Nearby diagnostic correlation uses account and timestamp, so tightly simultaneous sends can share the diagnostic window.

- `Carbon-IPHONE-CHECK-1` was stored at 01:01:11 UTC and completed; container logs recorded `reply_sent` at 01:01:16 UTC.
- `Carbon-MAC-CHECK-1` had no inbox row and no corresponding activity.
- This confirms the symptom. The pre-fix release has no rejected-message metadata, so the historical Mac event's precise cause cannot be proven from those logs alone.

## Concurrent work and rollout

This change was developed in a separate clone on `fix/whatsapp-linked-device-ingress`, based on `a11a1e3` (the completed admin-management update). The other session's checkout was not edited. Integrate this focused branch after normal checks; do not overwrite the other checkout with a whole directory snapshot. For live diagnosis, publish a uniquely tagged branch image and verify the currently deployed version before switching, preserving the existing data volume and WhatsApp session. Do not start a second connected Carbon process against the live data/session. A later release must include this commit to retain the fix.

## Confirmed live result

The user sent the second pair after deployment on September 27, 2026 UTC:

| Marker | Received | Intake diagnostic | Result |
| --- | --- | --- | --- |
| `Carbon-IPHONE-CHECK-2` | 01:32:25 UTC | `whatsapp_direct_message` | One inbox row, completed, `reply_sent` |
| `Carbon-MAC-CHECK-2` | 01:32:51 UTC | `whatsapp_linked_device` | One inbox row, completed, `reply_sent` |

The Mac diagnostic is emitted only when canonical sender/chat identities match but the original addresses differ. It confirms that this live Mac message would have been rejected by the old comparison. Both messages now appear in persisted Activity, and both replies were sent. This confirms the linked-device address mismatch as the cause of the reproduced failure.

The user also confirmed receiving both replies. Both live messages resolve to the same canonical account and Carbon participant.


## Planned reconnect recovery

Queued offline delivery is a separate known gap from linked-device identity normalization. The current admission policy still rejects `is_offline` messages. See the [queued-during-downtime TODO and restart/reconnect E2E acceptance](implementation-status.md#todo-whatsapp-messages-queued-during-downtime). This is planned work only, not an implemented fix.
