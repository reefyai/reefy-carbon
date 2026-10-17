# Admin requests from the web UI

Initial setup still seals one or more verified WhatsApp participants as approval admins. After setup, the Admin section lists the current admins and offers a separate participant selector with **Request admin approval**. Candidates must have sent Carbon a private WhatsApp message. When there are at least two admins, each admin row also has **Request removal**.

`POST /api/admins/requests` accepts only `{ "user": "<participant UUID>" }`. Existing management authentication and CSRF protection apply; deployments using Reefy authentication retain optional local password behavior. The endpoint stages only `add_approval_admin`, never grants access directly. `POST /api/admins/removals` accepts the same shape and stages only `remove_approval_admin`; the last admin cannot be removed. A current admin must approve the request-bound WhatsApp notice. Rejection, expiry, duplicate suppression, proposal quotas and target/identity revalidation use the existing permission proposal machinery. The notice explicitly identifies the web UI as its source.

Web requests use a reserved internal principal, `carbon:web-admin-requests`, rather than impersonating a WhatsApp admin. It has no WhatsApp identity, no approval authority and no group-management authority. Ordinary model tools cannot use this principal. Its only allowed proposals are adding or removing an approval admin through the protected web endpoints. Results remain visible in Admin approval requests; the new admin receives the existing notification after approval.

Regular users still cannot propose group creation. Approval admins and delegated group managers retain the existing group-management behavior.

`permission_directory` now returns `approval_available` independently from `approval_admins_visible`. Non-admin callers see an empty admin identity list but can distinguish hidden identities from missing configuration. Vetted authorization failures are returned to the model with useful explanations; unexpected internal errors remain generic.

Regression coverage includes web authentication/CSRF and strict request shape, staging without immediate promotion, verified candidates, duplicate proposals, rejection of non-admin approvals, successful existing-admin approval, reserved-principal isolation, hidden admin identities, regular-user group denial, and web selector state.