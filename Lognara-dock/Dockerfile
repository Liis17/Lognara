# Сборка из каталога Lognara-dock: docker build -t lognara-dock .

FROM node:24-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS build
ENV CARGO_BUILD_JOBS=2
ARG LOGNARA_RELEASE_VERSION
ARG LOGNARA_GIT_COMMIT
ENV LOGNARA_RELEASE_VERSION=$LOGNARA_RELEASE_VERSION LOGNARA_GIT_COMMIT=$LOGNARA_GIT_COMMIT
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin lognara-dock

FROM debian:bookworm-slim
# libssl3 нужен webauthn-rs.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/lognara-dock /etc/lognara-dock/auth \
    && chown -R 1000:1000 /var/lib/lognara-dock /etc/lognara-dock
COPY --from=build /src/target/release/lognara-dock /usr/local/bin/lognara-dock
COPY --from=web /web/dist /usr/share/lognara-dock/web
ENV LOGNARA_DOCK_LISTEN_ADDR=0.0.0.0:7403
EXPOSE 7403
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/lognara-dock"]
