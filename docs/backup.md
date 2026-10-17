# Complete Carbon backup boundary

**Decision: back up all Carbon-owned persistent data/configuration, without exclusions.** One Reefy volume mounts at `/data`, owned by UID 10001, with `backup: ["borgbase"]` and `excludes: []`. This is the default for all future subsystems, not just the current web password.

```text
/data/
  state/
    config.json             # operator configuration and connection settings
    secrets/                # web password and Carbon-owned API credentials
    carbon.sqlite               # identities, grants, 2PR, jobs, inbox/outbox, audit
    carbon.sqlite-wal           # include SQLite sidecars whenever present
    carbon.sqlite-shm
    whatsapp/               # linked-device identity, protocol store and session keys
    staging/                # durable in-progress content operations
    derived/                # extracted text, indexes and persisted caches
    logs/                   # application logs if retained on disk
  content/                  # internal Git repository for user/Carbon resources
    .git/                   # complete history, retention refs and local LFS objects
    .gitattributes          # Carbon-managed LFS rules
    resources/              # stable UUID paths; large media uses LFS
```

This is the target layout. The current starter still creates `/data/config.json` and `/data/secrets/admin-password`; migrate them explicitly into `/data/state` before adopting this layout, preserving credentials and refusing ambiguous conflicting state.

Most coordinator directories are planned, not yet created by the UI starter. Rebuildable data is included too; storage optimization/exclusions can be a separate future decision. Database paths, attachment staging, bundles and all durable queue/session storage must stay inside this boundary. Do not use symlinks or extra mounts to hide required state outside it.

Code, libraries, executable/UI assets and compiled dependencies stay in the container image; they are rebuilt/pulled, not mixed into data backups. Keep the matching image/version available for recovery. `/tmp` contains only disposable work. An accepted upload or acknowledged operation must be durable in the backed-up volume first. Process memory and stdout-only container logs are not persistent Carbon storage; if a log must be retained, write it under /data/state/logs/.

## Example: participant uploads and shared knowledge

Proposed storage layout, not yet implemented. IDs below are synthetic; names inside documents are domain content, not core application entities.

```text
/data/
  state/carbon.sqlite
  content/
    .git/                          # includes lfs/objects and retained history
    .gitattributes                 # application-controlled LFS routing
    resources/
      res_101/content.md           # p_001's profile notes
      res_102/content.md           # p_001's previous request/outcome
      res_103/content.md           # p_002's private notes
      res_104/content.jpg          # p_001's photo
      res_105/content.mp4          # p_001's video; large content uses LFS
      res_201/content.md           # shared reference notes
      res_202/content.pdf          # shared insurance document
  state/derived/
    res_104/<version>/caption.md
    res_202/<version>/extracted.md
```

SQLite maps each resource to its owner, logical folder/bundle, permissions and published Git commit/path. For example, res_104 is owned by p_001; res_202 grants read access to g_community. Git retains text/content revisions; large-file entries are LFS pointers whose actual bytes must be retained in the local LFS object store. Names such as profile.md and ac-photo.jpg are display names, not user-supplied storage paths. IDs here are synthetic stand-ins for UUIDs. See [content storage](content-storage.md).

The same data can be presented to participants as logical folders:

```text
Participant p_001 (private)
  profile.md
  previous-requests/
    request-001/
      notes.md
      ac-photo.jpg
      problem-video.mp4

Participant p_002 (private)
  profile.md

Shared bundle b_001 (readable by g_community)
  reference.md
  insurance.pdf
```

These are permissioned application folders, not a second duplicate filesystem tree. The directory index and parent relationships live in SQLite. New revisions retain history; each participant sees only permitted resources and versions. Shared resources have explicit audiences and editors; they are not automatically readable by every participant.

WhatsApp conversation messages, identity bindings and structured request/job state live in SQLite. Selected profile/summary notes can also be Markdown resources; uploaded attachments live in Git/LFS. Phone numbers are replaceable channel bindings, never directory names. Changing p_001's phone number does not move any content.

Physical directory names do not enforce tenant isolation. Carbon validates the acting participant's access before reads, searches, model-context assembly and sends; it never gives the model arbitrary filesystem paths or exposes this volume as static web content. Derived captions/extractions inherit their source access constraints.

Everything shown is within the same backed-up /data volume, including private and shared resources, metadata and derived files.

## Why /data?

**Decision: use `/data` as the container mount for all persistent Carbon state.** Its meaning is clear, and Reefy backs up declared mounts regardless of their name. Code, libraries and compiled UI assets remain in the image outside this mount.

Docker does not prescribe one universal container data path: its [volume guidance](https://docs.docker.com/engine/storage/volumes/) focuses on persistence outside the container writable layer. [PostgreSQL examples](https://docs.docker.com/get-started/docker-concepts/running-containers/persisting-container-data/) use `/var/lib/postgresql`, while [Redis](https://redis.io/docs/latest/operate/oss_and_stack/install/install-stack/docker/) uses `/data`.

CamAdmiral separates code in `/opt/camadmiral` from state in `/var/lib/camadmiral`. Carbon keeps that separation with a shorter data path. `/app/data` and `/var/lib/reefy-carbon` would also work; the name itself gives no backup, isolation or performance advantage. Do not mount a data volume over a directory containing required image code/assets, since a mount can obscure them.

This is a container convention, not strict host-filesystem packaging. Carbon deliberately colocates configuration, retained logs and secrets in `/data` so a single app-volume backup covers them. On the host, Reefy still chooses its own per-instance volume path.

### Carbon installations

Carbon uses its own instance data volume mounted at `/data`. It starts with fresh state and does not import another app’s database, configuration or WhatsApp session. Recreating a Carbon container preserves its existing Carbon volume and settings.

## Fit with Reefy

[Reefy's storage architecture](https://github.com/reefyai/reefy/blob/main/docs/storage-architecture.md) gives backup-enabled app volumes their own thin LV for snapshot backup on supported layouts. The host path is platform-managed per app instance; Carbon only uses its container mount path. Carbon does not need its own cron job or backup daemon. Actual remote backup still requires the device's BorgBase backup setup to be configured and successful; a manifest declaration alone is not proof of a completed backup.

One volume keeps Carbon state, protocol databases, configuration, keys and blobs in the same filesystem snapshot. Future write paths must durably store Git commits/objects, retention refs and required LFS bytes before publishing their references in SQLite. Include every database's WAL/sidecar files and use supported recovery semantics. A filesystem snapshot does not capture unflushed application memory or make multiple independent database writes one transaction; restore testing remains required.

A Git clone, Git push or repository-only archive is not a full backup: permissions and operational state are in SQLite, while LFS bytes are separate from Git pointers. Include the entire /data/content/.git directory and all retained local LFS objects; no automatic LFS pruning may discard SQLite-referenced history.

For manual raw copies, stop Carbon and copy the whole volume. Never back up only the main .sqlite file while it is live. Current launcher and Compose share the same named volume; use one runtime instance at a time.

## Restore contract

Restore into an offline, quarantined instance, including ownership and restrictive secret permissions, before starting normal Carbon participant access. Existing config and credentials take precedence over first-run initialization. A missing/failed restore must not be treated as a fresh empty deployment. Reefy's restore-failure startup guard is part of the platform contract; test it on the target device.

Start only the recovery surface with the matching application version, check SQLite integrity and every published/retained Git and LFS reference, then apply reviewed migrations if needed. Test restored identities, ACLs, reviewers, files, histories, jobs, session state and credentials. An old snapshot can contain pending sends/jobs, obsolete identity bindings and revoked grants: reconcile current authority before enabling any normal participant reads, searches, model requests or external effects. Pausing only scheduled sends does not protect against an inbound request using restored obsolete permissions. A restored WhatsApp session may have been revoked remotely and need re-linking; saved files cannot override that.

Restore detection must come from a trusted deployment/restore procedure, not a marker restored from the same snapshot. An ordinary offline database cannot know which grants were revoked after it was saved. Require an explicit recovery activation decision with conservative grant/binding reconciliation; automatic complete reconstruction would need a separate authoritative journal and is not claimed here. The concrete Reefy signal/operator procedure must be validated before release.

Only one installation instance may be active for a transport identity. A local volume lock prevents two processes using the same volume, but cannot fence a clone on another volume/device. Before takeover, stop/fence the old instance and re-verify or re-link/revoke provider sessions as needed; generating a new local epoch alone cannot stop the old device. Restore E2E tests use fake transport and cannot accidentally connect a second live WhatsApp session.

Carbon-owned secrets must be inside the backup boundary. Platform-issued credentials and the Reefy LLM-proxy service itself remain platform-owned and must be restored/reissued by Reefy; Carbon preserves its references and settings. External custom config/secret paths supported by the development binary do not qualify as a complete app backup unless relocated into this volume or covered by another explicitly declared backup volume.

Backup/restore acceptance is pending a real Reefy installation. The container smoke test checks volume persistence across recreation, not BorgBase recovery. Treat backup archives with the same access restrictions as live private content and credentials.
