#!/usr/bin/env bash
set -euo pipefail
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo 'Run this paid live-model test on an authorized Linux x86-64 build host, not the editing Mac.' >&2
  exit 2
fi
if [[ "${CARBON_E2E_LIVE:-}" != 1 ]]; then
  echo 'Set CARBON_E2E_LIVE=1 to authorize real model requests. WhatsApp remains simulated.' >&2
  exit 2
fi
case "${CARBON_E2E_SCENARIO:-ac}" in
  ac) live_test=live_grok_ac_journey ;;
  event) live_test=live_event_journey ;;
  *) echo 'CARBON_E2E_SCENARIO must be ac or event' >&2; exit 2 ;;
esac
export CARBON_E2E_ARTIFACT_DIR="${CARBON_E2E_ARTIFACT_DIR:-/tmp/carbon-ac-e2e-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
export CARBON_E2E_PROXY_URL="${CARBON_E2E_PROXY_URL:-http://127.0.0.1:9080/v1}"
export CARBON_E2E_MODEL="${CARBON_E2E_MODEL:-xai/grok-4.7}"
export CARBON_E2E_SOURCE_REV="$(git rev-parse HEAD)"
export CARBON_E2E_SOURCE_DIRTY="$(test -z "$(git status --porcelain)" && echo false || echo true)"
mkdir -p "$CARBON_E2E_ARTIFACT_DIR"
echo "${CARBON_E2E_SCENARIO:-ac} acceptance artifacts: $CARBON_E2E_ARTIFACT_DIR"
set +e
cargo test --locked "$live_test" -- --ignored --nocapture --test-threads=1 2>&1 | tee "$CARBON_E2E_ARTIFACT_DIR/runner.log"
result=${PIPESTATUS[0]}
set -e
if [[ -f "$CARBON_E2E_ARTIFACT_DIR/model-trace.json" ]]; then
  python3 scripts/render-e2e-trace.py "$CARBON_E2E_ARTIFACT_DIR"
fi
exit "$result"
