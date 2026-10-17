# Docker and Reefy packaging

Carbon follows CamAdmiral's packaging layout: root Dockerfile and Compose file, start/stop scripts, a VERSION file, and a Reefy manifest under reefy/. The runtime is a single Rust process; the fixed entrypoint only initializes missing standalone configuration and credentials before exec. None of these deployment commands is available to the LLM.

Carbon stores application state and configuration in `/data/state` and Git/LFS documents in `/data/content`. It is a separate app with fresh state; no other app’s data or WhatsApp session is imported.

## Run from source with Docker

```sh
docker build -t reefy-carbon:local .
CARBON_IMAGE=reefy-carbon:local ./start-carbon.sh
```

Open http://127.0.0.1:18082. Username is `admin`. Retrieve the generated password explicitly:

```sh
docker exec reefy-carbon cat /data/secrets/admin-password
```

The launcher waits for health, does not print passwords, and preserves the named `reefy-carbon-data` volume. `CARBON_BIND_IP` and `CARBON_PORT` select the standalone host binding; default is loopback port 18082 so it can coexist with CamAdmiral. For network access, put an authenticated HTTPS proxy in front; Basic auth over plain HTTP does not encrypt credentials.

```sh
./stop-carbon.sh
CARBON_IMAGE=reefy-carbon:local ./start-carbon.sh
# Rebuild first, then recreate with the existing volume:
docker build -t reefy-carbon:local .
CARBON_IMAGE=reefy-carbon:local ./start-carbon.sh --update
```

The launcher acquires/checks the new image before stopping an existing container. A failure after recreation begins is reported; automatic rollback is not implemented. Save the previous image reference for manual rollback, subject to future database-schema compatibility.

Without CARBON_IMAGE, the launcher pulls the immutable release tag specified in `start-carbon.sh`. An explicitly supplied CARBON_IMAGE must already exist locally. `--update` does not itself build a custom image.

Alternatively:

```sh
docker compose up -d --build
docker compose stop
```

Use either Compose or the launcher for a given volume, never both simultaneously: two instances must not share one WhatsApp session/database. Compose also uses `reefy-carbon-data`; do not use `down -v` unless intentionally deleting it.

## Runtime boundary

- Multi-stage build: Rust compiler in the build stage; executable, CA certificates and curl health probe in the runtime stage. No Node runtime.
- UID/GID 10001, read-only root filesystem, dropped capabilities, no-new-privileges in standalone launch, bounded /tmp, memory and process limits.
- Bridge networking, container port 18080. Carbon does not need CamAdmiral's host networking for camera discovery. Reefy allocates the device-facing port and provides authenticated HTTPS routing.
- Initial limits: 256 MiB and 128 processes. These are starter limits, not measured WhatsApp/LLM workload requirements; profile before rollout.
- SIGTERM/SIGINT gracefully stop the HTTP server. Future transport workers must flush durable state before shutdown completes.

## Configuration and persistence

The selected container data root is `/data`. See [existing development-volume migration](backup.md#existing-development-volumes) if using an earlier local build.

The sole default writable persistent boundary is `/data`. First start creates `state/config.json` and a 32-byte random password encoded as hex in `state/secrets/admin-password`, owned by the non-root app with restrictive permissions. Existing configuration/secrets win on restart, update and restore. Empty/broken existing configuration fails rather than regenerating credentials or disabling auth.

Keep managed configuration and Carbon-owned secrets inside the data volume. `CARBON_CONFIG_FILE` can select another file within that volume; missing configuration/secrets fail startup. The generic binary also supports external paths for local development, but that layout is not a complete Reefy backup: production deployment must relocate those files into the backed-up volume or explicitly declare another backup-enabled volume. Do not silently rely on host-only secret/config mounts. Container health assumes the management listener remains on 18080; update the health probe if overriding that port.

Planned layout as the coordinator is implemented:

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

Back up the entire volume without exclusions; see [backup policy](backup.md). Stop the app before a raw volume backup to keep future SQLite/session state consistent. Treat backups as secrets. Never regenerate restored identity/session material. Do not mount the Docker socket or host filesystem into Carbon.

## Reefy organization

`reefy/app.json` uses the [Reefy app contract](https://github.com/reefyai/reefy/blob/main/docs/reefy-apps.md) and [CamAdmiral layout](https://github.com/reefyai/reefy-camadmiral/blob/main/reefy/app.json):

- Stable slug `carbon`, display name `Carbon`.
- Date-based package version in VERSION and immutable versioned image reference/release entry.
- Container default_port 18080; no host-network exception.
- Data volume owned by UID 10001 with BorgBase backup enabled and an explicit empty exclusion list.
- `needs_llm: true` to request Reefy's network integration; the actual Carbon model-discovery adapter is still pending.
- No GPU, device access or platform capabilities requested.

Carbon is packaged for Linux amd64 and the Reefy app catalog. Each release must pass remote Linux checks and container smoke, publish the matching tag, and preserve the existing volume during upgrades. The current deployment target is x86-64 only; ARM64 is deferred. A real full-data backup/restore drill remains required before production use. REEFY_APP_VERSION takes precedence in the status API; otherwise the binary uses VERSION. No version/status panel is added to the UI.

## Validation

`cargo test --locked` and `cargo clippy --locked --all-targets -- -D warnings` validate Rust changes. `sh -n` checks deployment-script syntax. The GitHub workflow builds the image and runs `python3 tests/container_smoke.py reefy-carbon:test`, testing real HTTP authentication, non-root operation, graceful stop and preserved credentials after container recreation. It uses unique test containers/volumes and removes only those fixtures.

`experimental_tasks` and `debug_trace` are optional configuration settings, disabled by default. Standalone initialization uses a generated Basic-auth password; Reefy also protects the management endpoint. Direct edits to generated Compose are overwritten by reconciliation, so upgrades must use the managed instance-version API.