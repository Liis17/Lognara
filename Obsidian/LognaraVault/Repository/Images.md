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

`.github/workflows/publish-images.yml` разделяет сборку в `dev` и продвижение в `master`. Выборочные dev-сборки используют прежние контексты Dockerfile, Buildx и отдельный GHA-кеш компонента. Авторизация в registry использует `REGISTRY_USERNAME`/`REGISTRY_PASSWORD`, права packages не нужны.

`.github/workflows/publish-images.yml` передаёт в каждый core/relay/dock Dockerfile аргументы `LOGNARA_RELEASE_VERSION=dev-<полный SHA>` и `LOGNARA_GIT_COMMIT=<полный SHA>`. Dockerfile экспортирует значения до `cargo build`, поэтому изменение version/SHA меняет кешируемый build-layer. Core, relay heartbeat и Dock infrastructure API сохраняют package `version` и дополнительно отдают `release_version`; Dock и relay при пустом release arg используют Cargo package version и добавляют первые 7 символов SHA, если SHA задан. Без обоих args локальная сборка сообщает package version. UI инфраструктуры показывает release tag, а при его отсутствии package version с коротким SHA. Dev-образ получает `dev`, `dev-<полный SHA>` и `sha-<полный SHA>`, OCI revision исходного коммита; версия сохраняется при продвижении: стабильный тег registry является алиасом тех же байтов, а не новой сборкой.

На `master` `.github/scripts/promote_images.py` обрабатывает все три компонента. Скрипт фиксирует digest текущего `dev`-тега, читает OCI revision именно по digest и проверяет принадлежность коммита истории `master` и отсутствие изменений входов сборки после него. Для core учитываются core/agent/relay/spool, для relay — relay/spool, для dock — весь dock; общие настройки Rust, dockerignore и код публикации также участвуют в проверке. Отсутствующий образ, revision или неподходящая история останавливают выпуск. Перед merge promotion PR дождитесь успешного завершения всего CI на dev, включая публикацию образов: зелёный `CI passed` появляется до публикации. Если продвижение началось раньше готовности образов, повторите master-run после успешной dev-сборки. Если dev уже ушёл вперёд master, продвижение откажет: нужен следующий promotion PR. Если dev-сборка упала, последующий push без изменений компонента её не заменит; восстановление — `Release all containers` на актуальном dev, затем повтор продвижения.

При совпадении digest с `master` компонент пропускается. Иначе `next-image-version.sh` выбирает следующую независимую patch-версию среди числовых тегов registry (`0.0.1` для первого выпуска). Buildx `imagetools create --prefer-index=false` копирует manifest по digest под числовой тег, `master` и `sha-<SHA master>`, затем скрипт сверяет digest всех трёх тегов. Образ не пересобирается, исходный OCI revision и встроенная версия dev сохраняются. `latest`, контейнерные Git-теги и GitHub Releases не создаются.

Сборки и продвижение сериализованы общей для dev/master очередью `container-release-<component>`, `cancel-in-progress: false`, `queue: max`. Это защищает расчёт стабильной версии и теги от одновременного изменения в этом репозитории. Внешние издатели должны соблюдать ту же сериализацию. Выпуск трёх компонентов и обновление нескольких тегов не атомарны; повторный запуск пропустит уже совпадающие компоненты. Для пропущенного компонента новый SHA-алиас не создаётся; после частичной ошибки обновления тегов он также может отсутствовать, даже если master уже указывает на нужный digest.

## Ручной полный релиз

`Release all containers` на `dev` запускает полный CI и собирает все три образа; это также обязательная начальная загрузка dev-тегов после перехода. На `master` тот же workflow выполняет полные проверки и продвигает существующие dev-образы. Он не подменяет отсутствующую dev-сборку пересборкой master. Обычный ручной запуск `CI` только проверяет код. Права вызывающего reusable CI включают contents/pull-requests read и id-token write для декларации вложенного NuGet job; сам ручной контейнерный выпуск NuGet не публикует.

Для скачивания приватных образов выполните `docker login docker.barkfluff.com` на хосте развёртывания.

## Архивы агента

`.github/workflows/release-agent.yml` запускается при push в `master` и PR с изменениями agent, spool или самого workflow, а также вручную. PR проверяет обе архитектуры и smoke-тесты; публикация разрешена только вне PR на `master`. Нативные runners amd64/arm64 собирают musl release с `--locked`, проверяют форматирование, Clippy, тесты, память и отсутствие ELF-загрузчика/динамических зависимостей. Docker smoke-тест `Lognara-agent/tests/release_nginx.py` проверяет nginx Debian/Alpine, штатный entrypoint, доставку access/error logs, восстановление spool и остановку.

Файлы: `lognara-agent-linux-amd64.tar.gz`, `lognara-agent-linux-arm64.tar.gz`, `SHA256SUMS`; внутри каждого архива исполняемый `lognara-agent` и `LICENSE` в корне. Архив выбирается по целевой архитектуре контейнера, перед распаковкой проверяется SHA-256.

`Lognara-agent/scripts/release_agent.py` создаёт тег `agent-YYYYMMDD-HHMMSS-<12 символов SHA>` по UTC-времени коммита, не меняя Cargo.toml. Сначала создаётся draft, загружаются обе архитектуры и суммы, затем релиз публикуется. Опубликованный релиз при повторном запуске сохраняет исходные файлы. Публикация сериализована через очередь `queue: max` (лимит GitHub: 100 ожидающих jobs); старый коммит не получает latest, если уже опубликован его потомок.

Описание начинается коммитами agent/spool после предыдущего опубликованного релиза агента в истории текущего коммита; первый выпуск включает всю историю этих путей. Затем идут SHA и ссылки на архивы. В конце подставляется `Lognara-agent/docs/release-nginx.md`: Dockerfile скачивает архив по закреплённому тегу через TARGETARCH, проверяет сумму и копирует бинарник в nginx; Compose задаёт init, 30 секунд остановки, volume spool и runtime-параметры. Агент оборачивает штатный `/docker-entrypoint.sh` и собирает stdout/stderr. Relay должен быть доступен из сети nginx, персональный токен выдаётся в dock.

## Архивы хост-агента

Job `publish` в `.github/workflows/build-hwagent.yml` работает только для `master` (push или ручной запуск) после сборки обеих архитектур. Push запускает его при изменениях hwagent, agent, relay, spool и самого workflow: эти crates входят в бинарник.

Тег `hwagent-YYYYMMDD-HHMMSS-<12 символов SHA>` по UTC-времени коммита; префикс не совпадает с шаблоном `release_agent.py`, поэтому changelog и latest агента выпуски hwagent не учитывают. Файлы: `lognara-hwagent-linux-amd64.tar.gz`, `lognara-hwagent-linux-arm64.tar.gz`, `SHA256SUMS`; в архиве `lognara-hwagent`, `LICENSE`, `README.md`, `examples/` и `packaging/`. Как у агента, сначала создаётся draft, затем загружаются файлы и релиз публикуется; опубликованный релиз при повторе не меняется. Релиз публикуется с `--latest=false`, чтобы отметка Latest оставалась у агента. Описание короткое: SHA и ссылки на архивы, без списка коммитов.

## Релизы Android

Workflow .github/workflows/release-android.yml запускается при push в dev или master, если изменились lognara-android-client/**, сам workflow или .github/scripts/release_android.py. Ручной запуск выпускает только выбранную ветку dev или master. Проверки Android в CI описаны в [[Repository/CI#Android]].

Workflow запускает lint и testDebugUnitTest, затем собирает подписанный release APK и проверяет подпись через apksigner. Версия рассчитывается по UTC-времени коммита: versionCode = (секунды с 2025-01-01 00:00:00 UTC) / 60; versionName имеет формат YYYY.MM.DD.HHMM для stable и YYYY.MM.DD.HHMM-dev для dev. Теги: android-YYYYMMDD-HHMMSS-<12 символов SHA> и android-dev-YYYYMMDD-HHMMSS-<12 символов SHA>.

Master выпускает стабильный релиз с названием Lognara Android <versionName>; dev выпускает prerelease с названием Lognara Android dev <versionName>. Релиз сначала создаётся как draft, затем загружаются APK и метаданные, после чего он публикуется. Для обоих каналов latest=false, чтобы GitHub Latest оставался у релизов агента. Публикации идут через общую очередь без отмены. Повтор уже опубликованного коммита не перезаливает ассеты; draft можно продолжить.

Ассеты: lognara-android.apk, version.json и SHA256SUMS. В version.json указаны versionCode, versionName, channel, полный commit SHA, имя APK и SHA-256 APK. APK подписывается ключом из ANDROID_KEYSTORE_BASE64, ANDROID_KEYSTORE_PASSWORD, ANDROID_KEY_ALIAS и ANDROID_KEY_PASSWORD; workflow декодирует keystore во временный каталог runner и передаёт пути и пароли Gradle через ANDROID_SIGNING_*.

Changelog перечисляет коммиты с изменениями lognara-android-client/** после предыдущего опубликованного Android-релиза того же канала в истории текущего коммита; первый релиз включает всю историю этого пути. Затем приводятся исходный SHA, файлы и инструкции по установке и обновлению приложения.

## Связи

- [[Repository/CI]] — проверки, предшествующие публикации.
- [[Core/LognaraCore]], [[Relay/LognaraRelay]], [[Dock/LognaraDock]] — поставляемые сервисы.
- [[Agent/LognaraAgent]] — встраиваемый в контейнер приложения агент.
