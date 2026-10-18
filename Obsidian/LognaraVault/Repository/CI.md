# Continuous integration

Parent: [[Index]]

## Назначение

`.github/workflows/ci.yml` проверяет изменения на GitHub-hosted runner `ubuntu-24.04` после push в любую ветку, при открытии, обновлении и повторном открытии PR и через ручной запуск. Во всех ветках, включая `master`, запускаются только затронутые проверки с учётом зависимостей; ручной запуск проверяет всё. Workflow также доступен через `workflow_call` для полного релиза контейнеров. В рабочих ветках новый запуск для того же события и ветки/PR отменяет предыдущий незавершённый; запуски `master` не отменяют друг друга, чтобы не прерывать публикацию.

## Выбор проверок

Job `changes` использует `dorny/paths-filter@v4` после checkout с полной историей. Для PR список файлов получается через GitHub API относительно базовой ветки PR; для push в рабочую ветку сравнение идёт от merge-base с default branch. Для push в `master` сравнивается весь push с состоянием ветки до него. Учитываются удаления и оба пути переименований. `.github/scripts/select_ci.py` из совпавших фильтров формирует JSON-массивы Rust-crate и контейнеров, а также флаг веб-проверок.

| Изменения | Проверки |
|-----------|----------|
| `Lognara-agent/**` | agent и core (e2e использует agent). |
| `Lognara-relay/**` | relay и core (e2e использует relay). |
| `Lognara-spool/**` | spool, agent, relay и core (транзитивные зависимости). |
| `Lognara-core/**` | Только core. |
| `Lognara-dock/**`, кроме `web/**` | Только Rust dock. |
| `Lognara-dock/web/**` | Только web: TypeScript, сборка, unit- и браузерные тесты. |
| Корневые `.cargo/**`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain*`, `rustfmt.toml`, `.rustfmt.toml`, `clippy.toml`, `.clippy.toml` | Все проверки и образы. |
| `.dockerignore`, `.github/workflows/publish-images.yml`, `.github/scripts/next_image_version.py` | Все образы, без дополнительных Rust/web-проверок. |
| Остальные `.github/**` | Проверки автоматизации, без сборки незатронутых проектов. |
| Только файлы вне этих путей, например корневой README или Obsidian | Rust и web пропускаются; `CI passed` всё равно выполняется. |

Новая локальная зависимость в Cargo требует обновить соответствующие фильтры в workflow. Фронтенд использует поддельные API в браузерных тестах и не зависит от сборки Rust dock/core.

Контейнеры выбираются по тем же зависимостям: core при изменении core/agent/relay/spool, relay при изменении relay/spool, dock при любом изменении dock, включая web. Отсутствие изменений контейнеров пропускает публикацию. Изменения только корневого README или Obsidian не требуют сборок.

## Проверки

- Job `rust`: динамическая матрица затронутых crate из `Lognara-agent`, `Lognara-relay`, `Lognara-spool`, `Lognara-core`, `Lognara-dock`; при пустом списке job пропускается. Rust stable с rustfmt и Clippy. Проверяет форматирование, все targets/features через Clippy с `-D warnings`, обычную сборку без тестовых features и весь набор unit, интеграционных, e2e и doc-тестов с `--all-features`. Для сборки, Clippy и тестов используется `--locked`.
- Feature `crash-tests` у core активна в тестах, но не в обычной сборке.
- В матрице agent и relay дополнительно запускаются `tests/check-memory-profile.py`: нагрузочные тесты, пропускаемые обычным `cargo test`, с ограничением пикового RSS 256 MiB и 2048 MiB соответственно.
- Для agent также запускаются `tests/test_release_agent.py`: изолированные Git-истории для changelog, выбора предыдущего релиза и latest, первого выпуска и повторной публикации. Эти проверки выполняются и в PR.
- Job `web`: Node.js 24, `npm ci`, `npm run build` в `Lognara-dock/web`; команда проверяет TypeScript и собирает Vite. `npm test` запускает Node unit-тесты parser/history/live/CSV; после установки Chromium через Playwright job запускает `npm run test:e2e`. Браузерные сценарии используют настоящий React-интерфейс и поддельные API-ответы; отдельные Rust integration tests проверяют настоящий proxy dock.
- Job `automation`: всегда запускает стандартный Python unittest discovery в `.github/tests/`; проверяет выбор компонентов, расчёт версии, чтение страниц registry и ошибки авторизации/сети без обращения к рабочему registry.
- Job `ci` с именем проверки `CI passed`: всегда запускается после `changes`, `automation`, `rust` и `web`. Требует успеха выбора проверок, автоматизации и всех выбранных jobs, включая все элементы Rust-матрицы; для незатронутых jobs требует `skipped`. Ошибка или отмена обязательного job блокирует успех.
- Job `publish-images`: после успешного `ci` при push в `master` публикует только затронутые образы через reusable workflow `.github/workflows/publish-images.yml`. Отдельный `.github/workflows/release-containers.yml` (`Release all containers`) запускается вручную только для `master`, вызывает полный CI и выпускает все три контейнера. Версии, теги и очередь описаны в [[Repository/Images]]. Обычный ручной запуск `CI` проверяет всё без публикации.
- Отдельный workflow `Release agent` публикует архивы Linux amd64/arm64 при push в `master`, затрагивающем `Lognara-agent/**`. Он независимо проверяет musl targets (fmt, Clippy, release build, все тесты и память), Python-тесты подготовки релиза и nginx smoke-тесты. Публикация требует успеха обеих архитектур, только её job получает `contents: write`. Форматы архивов, changelog, повторные запуски и очередь публикации описаны в [[Repository/Images#Архивы агента]].

## Окружение

Cargo ограничен двумя параллельными заданиями компиляции; debug-информация dev/test отключена для экономии памяти и диска runner. Для Rust и npm настроены кеши зависимостей. Лимиты времени: выбор проверок 5 минут, Rust 45 минут, web 10 минут, итоговая проверка 5 минут, каждый образ 60 минут. Ошибка одного crate не отменяет проверки остальных. Проверки используют `contents: read`; только `changes` дополнительно получает `pull-requests: read` для списка файлов PR. Checkout не сохраняет credentials; секреты и внешние сервисы для тестов не нужны. Публикация входит в `docker.barkfluff.com:443` с `REGISTRY_USERNAME` и `REGISTRY_PASSWORD`, переданными в окружение из одноимённых GitHub Actions Secrets; права `packages: write` не требуются.

## Обязательная проверка PR

Для блокировки merge при ошибках администратор репозитория должен включить `CI passed` как обязательную проверку в branch protection или ruleset. Workflow публикует результат, но не меняет эти настройки. Локальный коммит запускает проверку только после push на GitHub.

## Зависимости

- Использует: [[Agent/LognaraAgent]], [[Relay/LognaraRelay]], [[Delivery/Spool]], [[Core/LognaraCore]], [[Dock/LognaraDock]].
- Используется в: [[Architecture]], [[Repository/RootFiles]], `README.md`.
