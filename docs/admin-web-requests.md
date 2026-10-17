# Admin requests from the web UI

Initial setup still seals one or more verified WhatsApp participants as approval admins. After setup, the Admin section lists the current admins and offers a separate participant selector with **Add admin**. Candidates must have sent Carbon a private WhatsApp message. When there are at least two admins, each admin row also has **Remove admin**. Both take effect immediately: the web console is the operator.

`POST /api/admins/requests` accepts only `{ "user": "<participant UUID>" }`. Existing management authentication and CSRF protection apply; deployments using Reefy authentication retain optional local password behavior. The endpoint applies `add_approval_admin` at once. `POST /api/admins/removals` accepts the same shape and applies `remove_approval_admin` at once. Both use the same identity checks, last-admin protection, context barrier, review-notice reconciliation and target notice as an approved WhatsApp change, and are recorded as approved requests in the audit log, marked as coming from the web UI. No WhatsApp approval is needed.

Web requests use a reserved internal principal, `carbon:web-admin-requests`, rather than impersonating a WhatsApp admin. It has no WhatsApp identity, no approval authority and no group-management authority. Ordinary model tools cannot use this principal. Its only allowed changes are adding or removing an approval admin through the protected web endpoints. Results remain visible in Admin approval requests; the affected participant receives the existing role-change notice.

Regular users still cannot propose group creation. Approval admins and delegated group managers retain the existing group-management behavior.

`permission_directory` now returns `approval_available` independently from `approval_admins_visible`. Non-admin callers see an empty admin identity list but can distinguish hidden identities from missing configuration. Vetted authorization failures are returned to the model with useful explanations; unexpected internal errors remain generic.

Regression coverage includes web authentication/CSRF and strict request shape, staging without immediate promotion, verified candidates, duplicate proposals, rejection of non-admin approvals, successful existing-admin approval, reserved-principal isolation, hidden admin identities, regular-user group denial, and web selector state.