# Managing approval admins (v2026.09.26-20)

After initial setup, an existing approval admin can ask Carbon through WhatsApp to add or remove an approval admin. For example: “Add +1 555 555 0123 as an approval admin.” The target must already have messaged Carbon privately and have one verified transport identity. Use `permission_directory` to resolve participants; the model must never invent IDs.

The model proposes `add_approval_admin {user}` or `remove_approval_admin {user}`. Rust checks the requester and produces a concise canonical approval notice naming the verified WhatsApp identity (phone/LID when native mapping is available). Any current approval admin may use WhatsApp Reply on that notice and send `approve` or `reject`, including the requester. Neither the proposal nor an LLM-generated approval grants authority. New admins cannot approve their own promotion before it has taken effect.

Group-management capability is separate: it does not permit changing or approving the approval-admin set. Approval admins receive no implicit read access to private notes, files or history. Removing the approval role does not delete the participant or remove independently granted group/resource permissions.

## Safeguards and handoff

- The last approval admin cannot be removed. Add a verified replacement first, then remove the old admin.
- At approval time, recheck current requester/reviewer authority, target status and exact verified identity, expiry and replay state.
- Per-participant role generations prevent pending requests surviving removal/re-addition of their requester or target. Independent additions for different people remain valid; adding one admin does not force a fresh request to add another.
- Role changes, generations, proposal decision, audit records and notification changes commit in one SQLite transaction. Concurrent removals cannot leave an empty set.
- Removed admins immediately lose approval authority, including on previously delivered request messages. Their queued review notices are cancelled.
- Newly added admins receive still-valid pending requests. Invalidated requests become stale and notify their requester; they cannot be approved from an old quote. This preserves the existing conservative global permission barrier, so some pending resource changes can become stale after an admin update.
- Existing sent or uncertain notices are not replayed when an admin is removed and re-added. Cancelled, unsent notices may be queued again when the request is still valid.
- The affected participant receives an informational role-change notice. This is not another approval step.

The web Admin approval section reflects the current set but stays read-only after initial setup. Subsequent changes go through identified WhatsApp participants. The bootstrap endpoint remains one-use; refreshing the page, restarting or removing an admin never reopens it.

Phone/LID labels cached from the native WhatsApp adapter are display metadata only. Labels cannot merge participants, relink identities or confer authority. Canonical notices freeze their displayed target when proposed; authorization remains bound to internal UUIDs and the exact verified transport identity. Labels and role state live under `/data/state`, within the existing whole-data backup.

## Remaining recovery work

This supports planned handoff while at least one admin can still use WhatsApp. It does not add a web override, lost-all-admins recovery, phone relinking or an uncertain-delivery retry interface. Those require a separate protected recovery contract. No live admin identities or roles are modified as part of deployment tests.

## Tests

Coverage includes unauthorized/group-admin escalation, unknown/ambiguous identities, proposed promotions remaining powerless, self-approval, last-admin protection, competing removals, non-conflicting additions, revoked quotes, pending-notice handoff, uncertain-send non-replay, role generations across restart and removal/re-add cycles, label/authority separation and unchanged private-resource access. A mock-model E2E goes from directory lookup to proposal, then an authenticated human quoted reply, and verifies the resulting role and access limits. UI tests cover live set changes and the sealed bootstrap controls.
