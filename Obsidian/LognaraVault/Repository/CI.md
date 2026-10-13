# Continuous integration

Parent: [[Index]]

## Назначение

`.github/workflows/ci.yml` проверяет изменения на GitHub-hosted runner `ubuntu-24.04` после push в любую ветку, при открытии, обновлении и повторном открытии PR и через ручной запуск. Фильтров по путям нет. Для одинакового события и ветки/PR новый запуск отменяет предыдущий незавершённый.

## Проверки

- Job `rust`: матрица отдельных crate `Lognara-agent`, `Lognara-relay`, `Lognara-spool`, `Lognara-core`, `Lognara-dock`; Rust stable с rustfmt и Clippy. Проверяет форматирование, все targets/features через Clippy с `-D warnings`, обычную сборку без тестовых features и весь набор unit, интеграционных, e2e и doc-тестов с `--all-features`. Для сборки, Clippy и тестов используется `--locked`.
- Feature `crash-tests` у core активна в тестах, но не в обычной сборке.
- В матрице agent и relay дополнительно запускаются `tests/check-memory-profile.py`: нагрузочные тесты, пропускаемые обычным `cargo test`, с ограничением пикового RSS 256 MiB и 2048 MiB соответственно.
- Job `web`: Node.js 24, `npm ci`, `npm run build` в `Lognara-dock/web`; команда проверяет TypeScript и собирает Vite. Frontend unit-тестов пока нет.
- Job `ci` с именем проверки `CI passed`: запускается после `rust` и `web` даже при ошибках и успешен только при успехе обоих jobs, включая все элементы Rust-матрицы.
- Job `publish-images`: только после успешного `ci` и только при push в `master`; собирает и публикует в GHCR образы core, relay и dock с тегами `master` и `sha-<полный SHA>`. Подробности в [[Repository/Images]].

## Окружение

Cargo ограничен двумя параллельными заданиями компиляции; debug-информация dev/test отключена для экономии памяти и диска runner. Для Rust и npm настроены кеши зависимостей. Лимиты времени: Rust 45 минут, web 10 минут, итоговая проверка 5 минут, каждый образ 60 минут. Ошибка одного crate не отменяет проверки остальных. Проверки используют только `contents: read`, checkout не сохраняет credentials; секреты и внешние сервисы для тестов не нужны. Публикация получает `packages: write` и входит в GHCR с автоматически выданным `GITHUB_TOKEN`; отдельный PAT не нужен.

## Обязательная проверка PR

Для блокировки merge при ошибках администратор репозитория должен включить `CI passed` как обязательную проверку в branch protection или ruleset. Workflow публикует результат, но не меняет эти настройки. Локальный коммит запускает проверку только после push на GitHub.

## Зависимости

- Использует: [[Agent/LognaraAgent]], [[Relay/LognaraRelay]], [[Delivery/Spool]], [[Core/LognaraCore]], [[Dock/LognaraDock]].
- Используется в: [[Architecture]], [[Repository/RootFiles]], `README.md`.
