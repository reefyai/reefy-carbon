#!/bin/sh
set -eu
IMAGE=${CARBON_IMAGE:-ghcr.io/reefyai/reefy-carbon:v2026.10.17-01}
CONTAINER=reefy-carbon
VOLUME=reefy-carbon-data
UPDATE=false
case $# in
    0) ;;
    1) [ "$1" = --update ] || { echo 'Usage: ./start-carbon.sh [--update]' >&2; exit 2; }; UPDATE=true ;;
    *) echo 'Usage: ./start-carbon.sh [--update]' >&2; exit 2 ;;
esac
command -v docker >/dev/null 2>&1 || { echo 'Docker is required.' >&2; exit 1; }
exists=false
docker container inspect "$CONTAINER" >/dev/null 2>&1 && exists=true
if [ "$exists" = true ] && [ "$UPDATE" = false ]; then
    docker start "$CONTAINER" >/dev/null
else
    # Acquire the new image before stopping a working installation.
    if [ -z "${CARBON_IMAGE:-}" ]; then
        docker pull "$IMAGE"
    else
        docker image inspect "$IMAGE" >/dev/null 2>&1 || { echo "Image not found: $IMAGE. Build or pull it first." >&2; exit 1; }
    fi
    docker volume create "$VOLUME" >/dev/null
    if [ "$exists" = true ]; then
        docker stop "$CONTAINER" >/dev/null
        docker rm "$CONTAINER" >/dev/null
    fi
    docker run -d --name "$CONTAINER" --restart unless-stopped \
        --read-only --cap-drop ALL --security-opt no-new-privileges:true \
        --memory 512m --memory-swap 512m --pids-limit 128 \
        --publish "${CARBON_BIND_IP:-127.0.0.1}:${CARBON_PORT:-18082}:18080" \
        --env OPENAI_BASE_URL --env OPENAI_API_KEY \
        --volume "$VOLUME:/data" \
        --tmpfs /tmp:rw,noexec,nosuid,size=16m,uid=10001,gid=10001 \
        "$IMAGE" >/dev/null
fi
# Do not print credentials until startup has succeeded.
attempt=0
while [ "$attempt" -lt 30 ]; do
    if docker exec "$CONTAINER" curl --fail --silent --max-time 2 http://127.0.0.1:18080/healthz >/dev/null 2>&1; then
        echo 'Carbon is running. Published address:'
        docker port "$CONTAINER" 18080/tcp
        echo 'Username: admin'
        echo 'Read the generated password with:'
        echo '  docker exec reefy-carbon cat /data/state/secrets/admin-password'
        exit 0
    fi
    attempt=$((attempt + 1))
    sleep 1
done
echo 'Carbon did not become healthy. Inspect: docker logs reefy-carbon' >&2
exit 1
