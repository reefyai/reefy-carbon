#!/usr/bin/env bash
# Real Carbon runtime and synthetic WhatsApp/documents; no outbound WhatsApp transport.
set -euo pipefail
case "${CARBON_E2E_LIVE:-0}" in
  0) mode=synthetic ;;
  1) mode=live
     : "${CARBON_E2E_PROXY_URL:?Set the authorized local DEV proxy URL}"
     : "${CARBON_E2E_MODEL:?Select the DEV model explicitly}" ;;
  *) echo 'CARBON_E2E_LIVE must be 0 or 1' >&2; exit 2 ;;
esac
artifact_dir="${CARBON_E2E_ARTIFACT_DIR:-$(mktemp -u "${TMPDIR:-/tmp}/carbon-sharing.XXXXXX")}"
# Each attempt keeps its own evidence; never overwrite an earlier result.
mkdir "$artifact_dir"
export CARBON_E2E_ARTIFACT_DIR="$artifact_dir"
export CARBON_E2E_SOURCE_REV="$(git rev-parse HEAD)"
export CARBON_E2E_VALIDATION_MODE="$mode"
finish() {
  result=$?
  trap - EXIT
  python3 - "$result" <<'PYTHON'
import json, os, pathlib, sys
path = pathlib.Path(os.environ['CARBON_E2E_ARTIFACT_DIR']) / 'result.json'
path.write_text(json.dumps({
    'source': os.environ['CARBON_E2E_SOURCE_REV'],
    'mode': os.environ['CARBON_E2E_VALIDATION_MODE'],
    'exit_code': int(sys.argv[1]),
    'model': os.environ.get('CARBON_E2E_MODEL') if os.environ['CARBON_E2E_VALIDATION_MODE'] == 'live' else 'mocked provider',
    'whatsapp_transport': 'synthetic',
    'customer_documents_used': False,
    'github_run_id': os.environ.get('GITHUB_RUN_ID'),
    'github_run_attempt': os.environ.get('GITHUB_RUN_ATTEMPT'),
}, indent=2) + '\n')
PYTHON
  exit "$result"
}
trap finish EXIT
if [[ "$mode" == live ]]; then
  cargo test --locked live_admin_document_sharing -- --ignored --nocapture --test-threads=1 2>&1 | tee "$artifact_dir/live.log"
else
  cargo test --locked direct_sharing -- --nocapture 2>&1 | tee "$artifact_dir/boundaries.log"
  cargo test --locked admin_document_sharing_e2e -- --nocapture 2>&1 | tee "$artifact_dir/journey.log"
  cargo test --locked admin_message_note_sharing_e2e -- --nocapture 2>&1 | tee "$artifact_dir/message-notes.log"
  cargo test --locked admin_everyone_sharing_e2e -- --nocapture 2>&1 | tee "$artifact_dir/everyone.log"
fi
