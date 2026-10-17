#!/bin/sh
set -eu
umask 077
# Fixed deployment initialization only; never called with model-generated commands.
if [ "$#" -gt 0 ]; then
    exec /usr/local/bin/reefy-carbon "$@"
fi
if [ -n "${CARBON_CONFIG_FILE:-}" ]; then
    # Explicit external configuration is authoritative; no fallback on error.
    exec /usr/local/bin/reefy-carbon "$CARBON_CONFIG_FILE"
fi
root=/data
mkdir -p "$root/state/secrets"
config="$root/state/config.json"
password="$root/state/secrets/admin-password"
if [ ! -e "$config" ]; then
    mkdir -p "$root/state/secrets"
    chmod 0700 "$root/state/secrets"
    if [ ! -e "$password" ]; then
        secret=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
        [ "${#secret}" -eq 64 ] || exit 1
        (set -C; printf '%s\n' "$secret" > "$password")
        unset secret
    fi
    [ -s "$password" ] || { echo 'Existing admin password is empty.' >&2; exit 1; }
    # Exclusive creation preserves existing configuration and credentials.
    (set -C; cat > "$config" <<'JSON'
{
  "listen": "0.0.0.0:18080",
  "auth": {
    "mode": "basic",
    "password_file": "/data/state/secrets/admin-password"
  }
}
JSON
    )
fi
exec /usr/local/bin/reefy-carbon "$config"
