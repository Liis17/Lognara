# Continuous integration

Parent: [[Index]]

## События и ветки

Основная разработка идёт через PR в `dev`: выбранные по путям Rust/web/.NET/Android-проверки и автоматизация должны завершиться успешно. PR в `master` допускается только из `dev` этого репозитория; guard `Promotion source` проверяет обе части. Такой PR не повторяет прикладные тесты, пройденные на входе в `dev`. Смена целевой ветки PR запускает проверку повторно (`edited`).

Push запускает CI только в `dev` и `master`. В `dev` выполняются выбор изменений, тесты автоматизации и сборка затронутых Docker-образов; прикладные проверки CI повторно не запускаются. Android отдельно тестируется и выпускается workflow `release-android` при изменении клиента на `dev` или `master` ([[Repository/Images#Релизы Android]]). В `master` выполняется продвижение готовых образов по digest; для изменённого .NET-пакета дополнительно выполняются обе ОС тестов и упаковка, чтобы NuGet-публикация получила проверенный артефакт текущего запуска. Релизы бинарников agent/hwagent остаются отдельными workflow для `master`.

Ручной запуск `CI` проверяет все компоненты без публикации. `Release all containers` на `dev` выполняет полные проверки и сборку всех контейнеров, на `master` — полные проверки и продвижение всех контейнеров. Прочие ветки для выпуска отклоняются. Конкурентные запуски одного события и ref идут очередью `queue: max` без отмены, чтобы не терять diff push.

## Выбор проверок

Job `changes` использует `dorny/paths-filter@v4` после checkout с полной историей. Для PR список файлов получается через GitHub API относительно базовой ветки PR; для push в `dev` и `master` сравнивается весь push с состоянием соответствующей ветки до него. Учитываются удаления и оба пути переименований. `.github/scripts/select_ci.py` из совпавших фильтров формирует JSON-массивы Rust-crate и контейнеров, а также флаги веб- и .NET-проверок.

| Изменения | Проверки |
|-----------|----------|
| `Lognara-agent/**` | agent и core (e2e использует agent). |
| `Lognara-relay/**` | relay и core (e2e использует relay). |
| `Lognara-spool/**` | spool, agent, relay и core (транзитивные зависимости). |
| `Lognara-core/**` | Только core. |
| `Lognara-dotnet/**`, `Lognara-agent/src/ingest.rs`, `Lognara-relay/src/normalize.rs`, `.github/workflows/ci.yml`, `.github/scripts/select_ci.py`, `.github/scripts/check_ci.py` | .NET sink на Ubuntu и Windows; контрактные Rust-файлы также запускают обычные зависимые проверки. |
| `Lognara-dock/**`, кроме `web/**` | Только Rust dock. |
| `Lognara-dock/web/**` | Только web: TypeScript, сборка, unit- и браузерные тесты. |
| Корневые `.cargo/**`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain*`, `rustfmt.toml`, `.rustfmt.toml`, `clippy.toml`, `.clippy.toml` | Все проверки и образы. |
| `.dockerignore`, `.github/workflows/publish-images.yml`, `.github/scripts/next-image-version.sh`, `.github/scripts/promote_images.py` | Все образы, без дополнительных Rust/web-проверок. |
| Остальные `.github/**` | Проверки автоматизации, без сборки незатронутых проектов. |
| Только файлы вне этих путей, например корневой README или Obsidian | Rust, web и .NET пропускаются; `CI passed` всё равно выполняется; push master сверяет digest всех контейнеров. |

Новая локальная зависимость в Cargo требует обновить соответствующие фильтры в workflow. Фронтенд использует поддельные API в браузерных тестах и не зависит от сборки Rust dock/core.

Для сборки в `dev` контейнеры выбираются по тем же зависимостям: core при изменении core/agent/relay/spool, relay при изменении relay/spool, dock при любом изменении dock, включая web. Отсутствие изменений контейнеров пропускает сборку в `dev`. В `master` проверяются все три контейнера, поскольку затронутые компоненты могли собираться в разных коммитах `dev`; уже совпадающий digest повторно не публикуется. Изменения только корневого README или Obsidian не требуют сборок.

## Проверки

- Job `rust`: динамическая матрица затронутых crate из `Lognara-agent`, `Lognara-relay`, `Lognara-spool`, `Lognara-core`, `Lognara-dock`; при пустом списке, push или promotion PR job пропускается; полный ручной запуск проверяет всё. Rust stable с rustfmt и Clippy. Проверяет форматирование, все targets/features через Clippy с `-D warnings`, обычную сборку без тестовых features и весь набор unit, интеграционных, e2e и doc-тестов с `--all-features`. Для сборки, Clippy и тестов используется `--locked`.
- Feature `crash-tests` у core активна в тестах, но не в обычной сборке.
- В матрице agent и relay дополнительно запускаются `tests/check-memory-profile.py`: нагрузочные тесты, пропускаемые обычным `cargo test`, с ограничением пикового RSS 256 MiB и 2048 MiB соответственно.
- Для agent также запускаются `tests/test_release_agent.py`: изолированные Git-истории для changelog, выбора предыдущего релиза и latest, первого выпуска и повторной публикации. Эти проверки выполняются и в PR.
- Job `web`: Node.js 24, `npm ci`, `npm run build` в `Lognara-dock/web`; команда проверяет TypeScript и собирает Vite. Затем `npm run typecheck:rs` дополнительно проверяет типы экспериментальным `tsc-rs` (Rust-порт tsc 7, версия закреплена точно) с `continue-on-error`: шаг только показывает расхождения с основным `tsc` и не блокирует CI. `npm test` запускает Node unit-тесты parser/history/live/CSV, версии и модели дерева инфраструктуры; после установки Chromium через Playwright job запускает `npm run test:e2e`. Браузерные сценарии проверяют дерево подключений, переименование, host-узел, ссылки на логи и прокрутку графа с настоящим React-интерфейсом и поддельными API-ответами; отдельные Rust integration tests проверяют настоящий proxy dock.
- Job `dotnet`: .NET SDK 8 (выбор закреплён в `Lognara-dotnet/global.json`) на `ubuntu-24.04` и `windows-2025`, restore с `--locked-mode`, Release-тесты и `dotnet pack`. Проверяет [[Agent/Serilog]], включая flush нескольких дисковых событий и восстановление очереди. Lock-файлы кешируются через setup-dotnet. Проверенный Ubuntu-пакет сохраняется как артефакт `serilog-nuget`.
- Job `publish-nuget`: только после успешного `ci` при push в `master`, если выбраны .NET-проверки. Условие содержит `!cancelled()` и явную проверку успеха `ci`/`changes`: пропуск необязательных Rust/web jobs не должен через неявный `success()` пропускать публикацию; ошибка CI и отмена блокируют её. Загружает проверенный артефакт текущего запуска без пересборки и публикует через `dotnet nuget push --skip-duplicate`; новая версия задаётся в csproj. Environment `nuget`, переменная `NUGET_USER`, доверенная политика NuGet.org для `ci.yml` и `NuGet/login@v1` обеспечивают OIDC без постоянного API-ключа. Только этот job получает `id-token: write`. Публикации сериализованы; PR, ручной запуск и вызов полного релиза контейнеров пакеты не публикуют. Настройка политики и права описаны в `Lognara-dotnet/README.md`.
- Job `automation`: в PR в dev, push и ручных запусках запускает стандартный Python unittest discovery в `.github/tests/`; проверяет выбор компонентов и скрипт релиза Android на изолированных Git-историях (версии, каналы, changelog, dry-run и повторную публикацию), без обращения к рабочему registry. Bash-скрипт расчёта версии образа отдельными тестами не покрыт.
- Job `ci` с именем `CI passed` всегда запускается после guard, выбора проверок и тестовых jobs. `.github/scripts/check_ci.py` требует успеха выбора, guard для PR и всех выбранных для события jobs; остальные должны быть `skipped`. Ошибки и отмены блокируют успех. Различия между PR в dev, promotion PR, push и полным запуском покрыты тестами `.github/tests/test_check_ci.py`.
- Job `publish-images` после успешного CI передаёт выбранные образы для `dev` или все три для `master` в `.github/workflows/publish-images.yml`. Workflow передаёт релизную метку и полный SHA каждого образа через build args; Dockerfile задаёт их до Cargo build для корректной инвалидации слоя. Публикация разрешена при push и полном ручном выпуске. Контракты продвижения описаны в [[Repository/Images]].
- Отдельный workflow `Release agent` публикует архивы Linux amd64/arm64 при push в `master`, затрагивающем `Lognara-agent/**`. Он независимо проверяет musl targets (fmt, Clippy, release build, все тесты и память), Python-тесты подготовки релиза и nginx smoke-тесты. Публикация требует успеха обеих архитектур, только её job получает `contents: write`. Форматы архивов, changelog, повторные запуски и очередь публикации описаны в [[Repository/Images#Архивы агента]].

## Android

Путь lognara-android-client/** выставляет флаг android в select_ci.py. Job android выполняет lint, testDebugUnitTest и assembleDebug только для PR в dev и полного ручного запуска; promotion PR и push пропускают его вместе с остальными прикладными CI-проверками. Job использует JDK 21 Temurin, Android SDK platforms;android-37 и build-tools 37.0.0, кеш Gradle и запускает команды через sh ./gradlew. CI passed требует успеха android, если он выбран для события, и проверяет, что в остальных случаях job пропущен.

Python-тесты скрипта подготовки Android-релиза запускаются job automation вместе с остальными тестами .github/tests. Сборка подписанного APK и публикация при push в dev/master описаны в [[Repository/Images#Релизы Android]].

## Окружение

Cargo ограничен двумя параллельными заданиями компиляции; debug-информация dev/test отключена для экономии памяти и диска runner. Для Rust, npm и NuGet настроены кеши зависимостей. .NET-проверки и публикация ограничены 10 минутами. Лимиты времени: выбор проверок 5 минут, Rust 45 минут, web 10 минут, итоговая проверка 5 минут, каждый образ 60 минут. Ошибка одного crate не отменяет проверки остальных. Проверки используют `contents: read`; только `changes` дополнительно получает `pull-requests: read` для списка файлов PR. Checkout не сохраняет credentials; секреты и внешние сервисы для тестов не нужны. Публикация входит в `docker.barkfluff.com` с `REGISTRY_USERNAME` и `REGISTRY_PASSWORD`, переданными в окружение из одноимённых GitHub Actions Secrets; права `packages: write` не требуются.

## Обязательная проверка PR

`.github/rulesets/required-tests.json` защищает `dev`: обязательный PR без обязательного approve, успешный `CI passed` от GitHub Actions и актуальность относительно базы. `.github/rulesets/promotion.json` защищает `master`: обязательный PR и `CI passed`, только merge commit без squash/rebase, без требования обновлять dev после каждого продвижения. Оба запрещают удаление и force push, bypass отсутствует. Проверка источника выполняется workflow, а не JSON ruleset.

`.github/scripts/set-required-tests.ps1` применяет эти rulesets через gh API, сохраняя остальные правила, включая `MasterBranch`. Скрипт до любых изменений требует развёрнутый promotion guard и скрипты на обеих ветках и наличие `CI passed` на обеих ветках. Порядок перехода: создать `dev` от актуального `master`, внести workflow через PR в `master` по прежним правилам, синхронизировать `dev` с этим коммитом, затем применить скрипт. Для первой сборки всех dev-образов запустить `Release all containers` на `dev`; затем использовать только PR в dev и promotion PR dev → master. До завершения перехода старое правило master сохраняется. JSON-файлы сами настройки GitHub не меняют.

## Зависимости

- Использует: [[Agent/LognaraAgent]], [[Relay/LognaraRelay]], [[Delivery/Spool]], [[Core/LognaraCore]], [[Dock/LognaraDock]].
- Используется в: [[Architecture]], [[Repository/RootFiles]], `README.md`.

Изменение `Lognara-dock/web/public/metrics-nginx.json` также запускает проверки Core: его тесты проверяют импортируемые nginx-регулярки тем же Rust regex, который использует извлечение.

## Хост-агент

`Lognara-hwagent` выбирается фильтром по путям hwagent, agent, relay и spool и проверяется тем же Rust-набором. Отдельный workflow `.github/workflows/build-hwagent.yml` (PR, push в `master`, ручной запуск) собирает static musl `lognara-hwagent` для amd64/arm64, проверяет отсутствие динамического загрузчика, упаковывает бинарник с примерами и systemd unit и загружает архивы как artifacts. Для `master` job `publish` выпускает их в GitHub Releases ([[Repository/Images#Архивы хост-агента]]).

Изменения `.github/workflows/ci.yml`, `.github/scripts/select_ci.py` и `.github/scripts/check_ci.py` выбирают полный набор проверок и образов. PR также обрабатывает событие `edited`, включая смену целевой ветки. Release agent проверяет PR в dev, изменения spool и своего workflow; публикация доступна только вне PR на master.
