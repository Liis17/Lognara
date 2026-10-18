### Запуск nginx со сбором логов

Скопируйте Dockerfile и compose.yaml в одну папку. Агент собирает stdout/stderr nginx, включая стандартные access/error logs, и сохраняет очередь в постоянном volume.

```dockerfile
# syntax=docker/dockerfile:1
FROM --platform=$BUILDPLATFORM alpine:3.22 AS download
ARG TARGETARCH
ARG LOGNARA_VERSION=@@TAG@@
WORKDIR /download
RUN apk add --no-cache curl
RUN set -eu; \
    case "$TARGETARCH" in amd64|arm64) ;; *) echo "Unsupported architecture: $TARGETARCH" >&2; exit 1;; esac; \
    base="https://github.com/@@REPOSITORY@@/releases/download/$LOGNARA_VERSION"; \
    archive="lognara-agent-linux-$TARGETARCH.tar.gz"; \
    curl -fSL --retry 3 "$base/$archive" -o "$archive"; \
    curl -fSL --retry 3 "$base/SHA256SUMS" -o SHA256SUMS; \
    grep "  $archive\$" SHA256SUMS > selected.sha256; \
    sha256sum -c selected.sha256; \
    tar -xzf "$archive"; \
    chmod 755 lognara-agent

FROM nginx:stable-alpine
COPY --from=download /download/lognara-agent /usr/local/bin/lognara-agent
ENTRYPOINT ["/usr/local/bin/lognara-agent", "run", "--", "/docker-entrypoint.sh"]
CMD ["nginx", "-g", "daemon off;"]
```

```yaml
services:
  nginx:
    build: .
    init: true
    stop_grace_period: 30s
    ports:
      - "8080:80"
    environment:
      LOGNARA_SERVICE: nginx
      LOGNARA_SERVER: docker-host
      LOGNARA_BACKEND: production
      LOGNARA_RELAY_URL: ${LOGNARA_RELAY_URL:?Задайте URL доступного relay с путём /v1/batches}
      LOGNARA_RELAY_TOKEN: ${LOGNARA_AGENT_TOKEN:?Задайте персональный токен из dock}
    volumes:
      - nginx-agent-spool:/var/lib/lognara-agent/spool
volumes:
  nginx-agent-spool:
```

Зарегистрируйте агента в dock для выбранного relay. Задайте `LOGNARA_AGENT_TOKEN` и `LOGNARA_RELAY_URL` в окружении или локальном `.env`, затем выполните `docker compose up --build -d` и `curl http://localhost:8080/`. Relay должен быть доступен из сети контейнера; `localhost` обозначает сам контейнер nginx. Для relay в другом Compose подключите nginx к его общей сети или используйте доступный адрес хоста.

Архитектура архива выбирается автоматически по целевой платформе Docker. Можно закрепить другую опубликованную сборку через `docker compose build --build-arg LOGNARA_VERSION=<тег>`. Сохраняйте volume при обновлениях. Штатный `/docker-entrypoint.sh` nginx выполняется перед запуском сервера; отдельная конфигурация файловых источников для стандартных логов не нужна.
