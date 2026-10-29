# Сборка из корня репозитория: docker build -f Lognara-relay/Dockerfile -t lognara-relay .

FROM rust:1-bookworm AS build
ENV CARGO_BUILD_JOBS=2
WORKDIR /src
COPY Lognara-relay/Cargo.toml Lognara-relay/Cargo.lock ./Lognara-relay/
COPY Lognara-relay/src ./Lognara-relay/src
COPY Lognara-spool/Cargo.toml ./Lognara-spool/
COPY Lognara-spool/src ./Lognara-spool/src
COPY Lognara-selflog/Cargo.toml ./Lognara-selflog/
COPY Lognara-selflog/src ./Lognara-selflog/src
RUN cargo build --release --locked --manifest-path Lognara-relay/Cargo.toml --bin lognara-relay

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/lognara-relay/spool \
    && chown -R 1000:1000 /var/lib/lognara-relay
COPY --from=build /src/Lognara-relay/target/release/lognara-relay /usr/local/bin/lognara-relay
ENV LOGNARA_LISTEN_ADDR=0.0.0.0:7401
EXPOSE 7401
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/lognara-relay"]
