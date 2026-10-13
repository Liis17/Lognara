# syntax=docker/dockerfile:1
FROM rust:1.94-bookworm AS rust-build
WORKDIR /build
ENV CARGO_TARGET_DIR=/build/target CARGO_BUILD_JOBS=2
COPY Lognara-spool/ Lognara-spool/
COPY Lognara-agent/ Lognara-agent/
COPY Lognara-relay/ Lognara-relay/
COPY Lognara-core/ Lognara-core/

FROM rust-build AS agent-build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --locked --release --manifest-path Lognara-agent/Cargo.toml --bin lognara-agent \
    && cp /build/target/release/lognara-agent /usr/local/bin/lognara-agent

FROM rust-build AS relay-build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --locked --release --manifest-path Lognara-relay/Cargo.toml --bin lognara-relay \
    && cp /build/target/release/lognara-relay /usr/local/bin/lognara-relay

FROM rust-build AS core-build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --locked --release --manifest-path Lognara-core/Cargo.toml --bin lognara-core \
    && cp /build/target/release/lognara-core /usr/local/bin/lognara-core

FROM python:3.13-slim-bookworm AS runtime
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1
RUN apt-get update && apt-get install -y --no-install-recommends libgcc-s1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

FROM runtime AS relay
COPY --from=relay-build /usr/local/bin/lognara-relay /usr/local/bin/lognara-relay
EXPOSE 7401
ENTRYPOINT ["lognara-relay"]

FROM runtime AS core
COPY --from=core-build /usr/local/bin/lognara-core /usr/local/bin/lognara-core
EXPOSE 7402
ENTRYPOINT ["lognara-core"]

FROM runtime AS app
WORKDIR /app
COPY --from=agent-build /usr/local/bin/lognara-agent /usr/local/bin/lognara-agent
COPY Example/server.py Example/entrypoint.py ./
EXPOSE 8881
ENTRYPOINT ["python", "/app/entrypoint.py"]
