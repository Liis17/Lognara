# Container images

Parent: [[Index]]

## Назначение

Core, relay и dock поставляются отдельными Docker-образами в `docker.barkfluff.com:443`. Agent поставляется статическими Linux-бинарниками в GitHub Releases для копирования в образ приложения; отдельный образ agent и образ библиотеки spool не публикуются.

## Сборка

- `Lognara-core/Dockerfile`: контекст корня репозитория; копирует core, spool и исходники path dev-зависимостей agent/relay для разрешения Cargo; собирает только `lognara-core` в release без `crash-tests`.
- `Lognara-relay/Dockerfile`: контекст корня; копирует relay и spool, собирает `lognara-relay` в release.
- `Lognara-dock/Dockerfile`: контекст `Lognara-dock`; отдельно собирает React на Node.js 24 и Rust-бинарник. Runtime включает `libssl3` для WebAuthn.
- `.dockerignore` корня и `Lognara-dock/.dockerignore` исключают локальные артефакты и `.env` из контекста.

Rust-стадии используют `rust:1-bookworm`, Cargo `--locked` и не более двух параллельных задач компиляции. Runtime основан на `debian:bookworm-slim`, работает от `1000:1000`, каталоги данных в образе принадлежат этому пользователю. Core, relay и dock слушают `0.0.0.0` на портах 7402, 7401 и 7403 соответственно. Данные и секреты передаются при запуске; постоянные volumes обязательны. Bind-папки на хосте должны быть доступны UID/GID `1000:1000`.

## Публикация и имена

Job `publish-images` в `.github/workflows/ci.yml` запускается только при push в `master` после успеха `CI passed`. Матрица собирает три образа для `linux/amd64`; Buildx использует независимый GHA-кеш для каждого компонента. Job публикации получает `REGISTRY_USERNAME` и `REGISTRY_PASSWORD` в окружении из одноимённых GitHub Actions Secrets; авторизация выполняется через `docker/login-action`. Права `packages: write` не требуются. PR, другие ветки и ручной запуск не публикуют образы. OCI labels связывают пакет с исходным репозиторием и коммитом.

Имена: `docker.barkfluff.com:443/lognara-core`, `docker.barkfluff.com:443/lognara-relay`, `docker.barkfluff.com:443/lognara-dock`. Имена не зависят от владельца GitHub-репозитория.

Каждый образ получает `master` и `sha-<полный SHA коммита>`. Первый тег обновляется; второй обозначает исходный коммит. Для согласованной поставки задаётся один `LOGNARA_IMAGE_TAG` в `.env` для всех компонентов. Повторная сборка коммита может изменить содержимое при обновлении базовых образов; digest `@sha256:...` фиксирует точное содержимое.

Предложение для будущих релизов: общая версия всех компонентов, Git-тег `v0.1.0` и тег образа `0.1.0`; исправления `0.1.1`, следующие возможности `0.2.0`. Текущий workflow не обрабатывает релизные теги и не публикует `latest`.

Для скачивания приватных образов выполните `docker login docker.barkfluff.com:443` на хосте развёртывания.

## Архивы агента

`.github/workflows/release-agent.yml` запускается только при push в `master` с изменениями `Lognara-agent/**`. Изменения только spool или workflow выпуск не запускают. Нативные runners amd64/arm64 собирают musl release с `--locked`, проверяют форматирование, Clippy, тесты, память и отсутствие ELF-загрузчика/динамических зависимостей. Docker smoke-тест `Lognara-agent/tests/release_nginx.py` проверяет nginx Debian/Alpine, штатный entrypoint, доставку access/error logs, восстановление spool и остановку.

Файлы: `lognara-agent-linux-amd64.tar.gz`, `lognara-agent-linux-arm64.tar.gz`, `SHA256SUMS`; внутри каждого архива исполняемый `lognara-agent` и `LICENSE` в корне. Архив выбирается по целевой архитектуре контейнера, перед распаковкой проверяется SHA-256.

`Lognara-agent/scripts/release_agent.py` создаёт тег `agent-YYYYMMDD-HHMMSS-<12 символов SHA>` по UTC-времени коммита, не меняя Cargo.toml. Сначала создаётся draft, загружаются обе архитектуры и суммы, затем релиз публикуется. Опубликованный релиз при повторном запуске сохраняет исходные файлы. Публикация сериализована через очередь `queue: max` (лимит GitHub: 100 ожидающих jobs); старый коммит не получает latest, если уже опубликован его потомок.

Описание начинается коммитами agent/spool после предыдущего опубликованного релиза агента в истории текущего коммита; первый выпуск включает всю историю этих путей. Затем идут SHA и ссылки на архивы. В конце подставляется `Lognara-agent/docs/release-nginx.md`: Dockerfile скачивает архив по закреплённому тегу через TARGETARCH, проверяет сумму и копирует бинарник в nginx; Compose задаёт init, 30 секунд остановки, volume spool и runtime-параметры. Агент оборачивает штатный `/docker-entrypoint.sh` и собирает stdout/stderr. Relay должен быть доступен из сети nginx, персональный токен выдаётся в dock.

## Связи

- [[Repository/CI]] — проверки, предшествующие публикации.
- [[Core/LognaraCore]], [[Relay/LognaraRelay]], [[Dock/LognaraDock]] — поставляемые сервисы.
- [[Agent/LognaraAgent]] — встраиваемый в контейнер приложения агент.
