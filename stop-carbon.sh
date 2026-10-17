#!/bin/sh
set -eu
command -v docker >/dev/null 2>&1 || { echo 'Docker is required.' >&2; exit 1; }
if ! docker container inspect reefy-carbon >/dev/null 2>&1; then
    echo 'Carbon is not installed.'
    exit 0
fi
docker stop reefy-carbon >/dev/null
echo 'Carbon stopped. Container, configuration, credentials and reefy-carbon-data are preserved.'
