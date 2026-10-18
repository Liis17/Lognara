# Container images

Parent: [[Index]]

## Назначение

Core, relay и dock поставляются отдельными Docker-образами в `docker.barkfluff.com`. Agent поставляется статическими Linux-бинарниками в GitHub Releases для копирования в образ приложения; отдельный образ agent и образ библиотеки spool не публикуются.

## Сборка

- `Lognara-core/Dockerfile`: контекст корня репозитория; копирует core, spool и исходники path dev-зависимостей agent/relay для разрешения Cargo; собирает только `lognara-core` в release без `crash-tests`.
- `Lognara-relay/Dockerfile`: контекст корня; копирует relay и spool, собирает `lognara-relay` в release.
- `Lognara-dock/Dockerfile`: контекст `Lognara-dock`; отдельно собирает React на Node.js 24 и Rust-бинарник. Runtime включает `libssl3` для WebAuthn.
- `.dockerignore` корня и `Lognara-dock/.dockerignore` исключают локальные артефакты и `.env` из контекста.

Rust-стадии используют `rust:1-bookworm`, Cargo `--locked` и не более двух параллельных задач компиляции. Runtime основан на `debian:bookworm-slim`, работает от `1000:1000`, каталоги данных в образе принадлежат этому пользователю. Core, relay и dock слушают `0.0.0.0` на портах 7402, 7401 и 7403 соответственно. Данные и секреты передаются при запуске; постоянные volumes обязательны. Bind-папки на хосте должны быть доступны UID/GID `1000:1000`.

## Публикация и имена

Job `publish-images` в `.github/workflows/ci.yml` при push в `master` после успеха `CI passed` передаёт список затронутых контейнеров в reusable workflow `.github/workflows/publish-images.yml`. Правила выбора описаны в [[Repository/CI]]. Матрица собирает образы для `linux/amd64`; Buildx использует независимый GHA-кеш для каждого компонента. Job публикации получает `REGISTRY_USERNAME` и `REGISTRY_PASSWORD` в окружении из одноимённых GitHub Actions Secrets; авторизация выполняется через `docker/login-action`. Права `packages: write` не требуются. PR и другие ветки не публикуют образы. OCI labels связывают образ с исходным репозиторием, коммитом и числовой версией.

Имена: `docker.barkfluff.com/lognara-core`, `docker.barkfluff.com/lognara-relay`, `docker.barkfluff.com/lognara-dock`. Имена не зависят от владельца GitHub-репозитория.

Перед сборкой `.github/scripts/next-image-version.sh` (bash, curl, jq) получает теги соответствующего репозитория через Registry HTTP API V2, включая все страницы. Среди тегов строгого формата `major.minor.patch` выбирается наибольшая числовая версия и увеличивается patch. У каждого компонента независимая последовательность: первая версия `0.0.1`, после `1.2.9` будет `1.2.10`. `master`, SHA, prerelease и теги с префиксом `v` не участвуют. Отсутствующий репозиторий (`404 NAME_UNKNOWN`) или отсутствие числовых тегов означает первый релиз; ошибки сети, авторизации и некорректный ответ останавливают публикацию. Используется Basic-аутентификация, только HTTPS. Адрес registry указывается без `:443`: registry возвращает адрес загрузки blob без порта, и Buildx с логином на `docker.barkfluff.com:443` отправляет такой PUT без учётных данных, получая 401.

Чтение версии, сборка и push находятся в общей для автоматического и ручного запуска concurrency-группе каждого компонента с `cancel-in-progress: false` и `queue: max`. Это исключает одновременный выбор одного номера внутри репозитория; очередь вмещает до 100 ожидающих jobs. Внешние издатели должны соблюдать ту же сериализацию. Повторный запуск после успешной публикации создаёт следующую patch-версию.

Каждый образ получает числовой тег релиза, `master` и `sha-<полный SHA коммита>`. `master` обновляется при каждом выпуске компонента; SHA обозначает исходный коммит. При выборочном выпуске SHA нового коммита есть только у выпущенных образов. Для отдельных версий задавайте числовой тег в `image` каждого сервиса; общий `LOGNARA_IMAGE_TAG` в примерах подходит для `master` или общего SHA полного релиза. Digest `@sha256:...` фиксирует точное содержимое; повторная сборка коммита может изменить его при обновлении базовых образов. Git-теги, GitHub Releases для контейнеров и `latest` не создаются.

## Ручной полный релиз

В GitHub Actions выберите `Release all containers` → `Run workflow` → ветку `master`. `.github/workflows/release-containers.yml` проверяет ветку и вызывает `ci.yml` с `release_all: true`: полные Rust/web-проверки и автоматизация, затем публикация core, relay и dock независимо от изменённых путей. Каждый компонент получает собственную следующую версию. Итог каждой успешной публикации записывается в GitHub Actions job summary. Релиз трёх образов не атомарен: при ошибке одного остальные могут быть уже опубликованы.

Для скачивания приватных образов выполните `docker login docker.barkfluff.com` на хосте развёртывания.

## Архивы агента

`.github/workflows/release-agent.yml` запускается только при push в `master` с изменениями `Lognara-agent/**`. Изменения только spool или workflow выпуск не запускают. Нативные runners amd64/arm64 собирают musl release с `--locked`, проверяют форматирование, Clippy, тесты, память и отсутствие ELF-загрузчика/динамических зависимостей. Docker smoke-тест `Lognara-agent/tests/release_nginx.py` проверяет nginx Debian/Alpine, штатный entrypoint, доставку access/error logs, восстановление spool и остановку.

Файлы: `lognara-agent-linux-amd64.tar.gz`, `lognara-agent-linux-arm64.tar.gz`, `SHA256SUMS`; внутри каждого архива исполняемый `lognara-agent` и `LICENSE` в корне. Архив выбирается по целевой архитектуре контейнера, перед распаковкой проверяется SHA-256.

`Lognara-agent/scripts/release_agent.py` создаёт тег `agent-YYYYMMDD-HHMMSS-<12 символов SHA>` по UTC-времени коммита, не меняя Cargo.toml. Сначала создаётся draft, загружаются обе архитектуры и суммы, затем релиз публикуется. Опубликованный релиз при повторном запуске сохраняет исходные файлы. Публикация сериализована через очередь `queue: max` (лимит GitHub: 100 ожидающих jobs); старый коммит не получает latest, если уже опубликован его потомок.

Описание начинается коммитами agent/spool после предыдущего опубликованного релиза агента в истории текущего коммита; первый выпуск включает всю историю этих путей. Затем идут SHA и ссылки на архивы. В конце подставляется `Lognara-agent/docs/release-nginx.md`: Dockerfile скачивает архив по закреплённому тегу через TARGETARCH, проверяет сумму и копирует бинарник в nginx; Compose задаёт init, 30 секунд остановки, volume spool и runtime-параметры. Агент оборачивает штатный `/docker-entrypoint.sh` и собирает stdout/stderr. Relay должен быть доступен из сети nginx, персональный токен выдаётся в dock.

## Связи

- [[Repository/CI]] — проверки, предшествующие публикации.
- [[Core/LognaraCore]], [[Relay/LognaraRelay]], [[Dock/LognaraDock]] — поставляемые сервисы.
- [[Agent/LognaraAgent]] — встраиваемый в контейнер приложения агент.
