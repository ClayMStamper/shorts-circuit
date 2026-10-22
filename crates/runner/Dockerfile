# The cloud runner image. Build from the repository root:
#   docker build -f crates/runner/Dockerfile -t shorts-runner .
# See docs/cloud-runner.md for running it with PostgreSQL behind a TLS proxy.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p shorts-runner \
    && cp target/release/shorts-runner /usr/local/bin/shorts-runner

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/shorts-runner runner \
    && mkdir -p /var/lib/shorts-runner \
    && chown runner /var/lib/shorts-runner
COPY --from=build /usr/local/bin/shorts-runner /usr/local/bin/shorts-runner
USER runner
# Inside the container it listens on all interfaces; publish the port to 127.0.0.1 only and
# put a TLS-terminating proxy in front (the compose example does both).
ENV SHORTS_RUNNER_BIND=0.0.0.0:8787 \
    SHORTS_RUNNER_DATA_DIR=/var/lib/shorts-runner \
    RUST_LOG=info,sqlx=warn
VOLUME /var/lib/shorts-runner
EXPOSE 8787
ENTRYPOINT ["shorts-runner"]
CMD ["serve"]
