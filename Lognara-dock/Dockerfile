# Сборка из корня: docker build -f Lognara-dock/Dockerfile -t lognara-dock .

FROM node:24-bookworm-slim AS web
WORKDIR /web
COPY Lognara-dock/web/package.json Lognara-dock/web/package-lock.json ./
RUN npm ci
COPY Lognara-dock/web/ ./
RUN npm run build

FROM rust:1-bookworm AS build
ENV CARGO_BUILD_JOBS=2
WORKDIR /src
COPY Lognara-dock/Cargo.toml Lognara-dock/Cargo.lock ./Lognara-dock/
COPY Lognara-dock/src ./Lognara-dock/src
COPY Lognara-selflog/Cargo.toml ./Lognara-selflog/
COPY Lognara-selflog/src ./Lognara-selflog/src
COPY Lognara-spool/Cargo.toml ./Lognara-spool/
COPY Lognara-spool/src ./Lognara-spool/src
RUN cargo build --release --locked --manifest-path Lognara-dock/Cargo.toml --bin lognara-dock

FROM debian:bookworm-slim
# libssl3 нужен webauthn-rs.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/lognara-dock /etc/lognara-dock/auth \
    && chown -R 1000:1000 /var/lib/lognara-dock /etc/lognara-dock
COPY --from=build /src/Lognara-dock/target/release/lognara-dock /usr/local/bin/lognara-dock
COPY --from=web /web/dist /usr/share/lognara-dock/web
ENV LOGNARA_DOCK_LISTEN_ADDR=0.0.0.0:7403
EXPOSE 7403
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/lognara-dock"]
