FROM rust:1.98.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock VERSION ./
COPY src ./src
COPY prompts ./prompts
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 carbon \
    && useradd --uid 10001 --gid 10001 --no-create-home carbon \
    && mkdir -p /data /etc/reefy-carbon \
    && chown 10001:10001 /data
COPY --from=build /build/target/release/reefy-carbon /usr/local/bin/reefy-carbon
COPY scripts/container-entrypoint.sh /usr/local/bin/carbon-entrypoint
COPY VERSION /etc/reefy-carbon/VERSION
RUN chmod 0755 /usr/local/bin/carbon-entrypoint
USER 10001:10001
VOLUME ["/data"]
EXPOSE 18080
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
    CMD curl --fail --silent --max-time 2 http://127.0.0.1:18080/healthz >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/carbon-entrypoint"]

LABEL org.opencontainers.image.source="https://github.com/reefyai/reefy-carbon"
LABEL org.opencontainers.image.title="Carbon"
