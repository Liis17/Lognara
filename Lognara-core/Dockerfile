# Сборка из корня репозитория: docker build -f Lognara-core/Dockerfile -t lognara-core .

FROM rust:1-bookworm AS build
ENV CARGO_BUILD_JOBS=2
WORKDIR /src
COPY Lognara-core/Cargo.toml Lognara-core/Cargo.lock ./Lognara-core/
COPY Lognara-core/src ./Lognara-core/src
# Cargo разрешает также path dev-зависимости, но их бинарники в образ не входят.
COPY Lognara-agent/Cargo.toml ./Lognara-agent/
COPY Lognara-agent/src ./Lognara-agent/src
COPY Lognara-relay/Cargo.toml ./Lognara-relay/
COPY Lognara-relay/src ./Lognara-relay/src
COPY Lognara-spool/Cargo.toml ./Lognara-spool/
COPY Lognara-spool/src ./Lognara-spool/src
COPY Lognara-selflog/Cargo.toml ./Lognara-selflog/
COPY Lognara-selflog/src ./Lognara-selflog/src
RUN cargo build --release --locked --manifest-path Lognara-core/Cargo.toml --bin lognara-core

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/lognara-core \
    && chown 1000:1000 /var/lib/lognara-core
COPY --from=build /src/Lognara-core/target/release/lognara-core /usr/local/bin/lognara-core
ENV LOGNARA_LISTEN_ADDR=0.0.0.0:7402
EXPOSE 7402
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/lognara-core"]
