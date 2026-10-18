# Корневые файлы

Parent: [[Index]]

## Назначение

Описывает корневые файлы репозитория Lognara.

## Файлы

- `README.md` — описание проекта на русском: назначение, схема потока логов (Mermaid), возможности компонентов, быстрый старт, порты и параметры `LOGNARA_*`, персональные токены relay/агентов через dock, метрики (ServiceMetrics и правила regex), `.env`, два фрагмента Compose (core+dock и relay+агенты), общий RAM relay и mem_limit, durable ACK и обязательное хранилище agent, relay и core (папка на хосте или Docker volume), порядок обновления, HTTP API core, эксплуатация, разработка, структура репозитория, статус и ограничения. Источники и запуск программ описаны в `Lognara-agent/docs/log-sources.md`, примеры в `Lognara-agent/examples/log-sources/`. Детали core вынесены в `Lognara-core/README.md` и `Lognara-core/docs/`.
- `.gitignore` — содержит исключения для Cargo, артефактов сборки агента в `/build/`, rustfmt, PDB, Cargo Mutants, RustRover, `default.profraw`, `.DS_Store`, локального `.env` с ключом, а также `node_modules/` и `Lognara-dock/web/dist/`.
- `.env.example` — центральные admin/query-токены `LOGNARA_CORE_ADMIN_TOKEN`, `LOGNARA_CORE_QUERY_TOKEN`, query-токен отдельного dock `LOGNARA_QUERY_TOKEN`, данные core; для машины приложений `LOGNARA_RELAY_CORE_TOKEN`, `LOGNARA_AGENT_TOKEN` из dock, `LOGNARA_RELAY_MEMORY_MB=2048`, данные agent/relay. Бинарники получают секреты через соответствующие `environment` поля Compose, `.env` напрямую не читают.
- `LICENSE` — лицензия MIT с указанием copyright 2026 Li_is.
- `.github/workflows/ci.yml` — GitHub Actions для всех веток и PR: сборка и тесты пяти Rust-crate, Clippy, форматирование, проверки памяти agent/relay и сборка веб-интерфейса. Детали в [[Repository/CI]].
- `.dockerignore` — исключает git, локальные Rust/Node-сборки и `.env` из корневого контекста Docker-сборки core и relay. Поставка в [[Repository/Images]].

`.env.example` также задаёт `LOGNARA_IMAGE_TAG=master` для выбора тегов core, relay и dock в примерах Compose; общий `sha-<полный SHA коммита>` можно использовать после полного релиза. Числовые patch-версии компонентов независимы; при их закреплении тег задаётся в `image` каждого сервиса. Контракт публикации описан в [[Repository/Images]].

## Ключевые методы/функции

Не применимо: корневые файлы не объявляют методы.

## Зависимости

- Использует: нет.
- Используется в: [[Architecture]].

## Важные детали

Агент реализован отдельным crate в `Lognara-agent/`; в корне общего Cargo workspace нет.
