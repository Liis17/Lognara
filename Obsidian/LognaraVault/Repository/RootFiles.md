# Корневые файлы

Parent: [[Index]]

## Назначение

Описывает корневые файлы репозитория Lognara.

## Файлы

- `README.md` — описание проекта на русском: назначение, схема потока логов (Mermaid), возможности компонентов, быстрый старт, порты и параметры `LOGNARA_*`, персональные токены relay/агентов через dock, `.env`, два фрагмента Compose (core+dock и relay+агенты), общий RAM relay и mem_limit, durable ACK и обязательное хранилище agent, relay и core (папка на хосте или Docker volume), порядок обновления, HTTP API core, эксплуатация, разработка, структура репозитория, статус и ограничения. Источники и запуск программ описаны в `Lognara-agent/docs/log-sources.md`, примеры в `Lognara-agent/examples/log-sources/`. Детали core вынесены в `Lognara-core/README.md` и `Lognara-core/docs/`.
- `.gitignore` — содержит исключения для Cargo, артефактов сборки агента в `/build/`, rustfmt, PDB, Cargo Mutants, RustRover, `default.profraw`, `.DS_Store`, локального `.env` с ключом, а также `node_modules/` и `Lognara-dock/web/dist/`.
- `.env.example` — центральные admin/query-токены `LOGNARA_CORE_ADMIN_TOKEN`, `LOGNARA_CORE_QUERY_TOKEN`, query-токен отдельного dock `LOGNARA_QUERY_TOKEN`, данные core; для машины приложений `LOGNARA_RELAY_CORE_TOKEN`, `LOGNARA_AGENT_TOKEN` из dock, `LOGNARA_RELAY_MEMORY_MB=2048`, данные agent/relay. Бинарники получают секреты через соответствующие `environment` поля Compose, `.env` напрямую не читают.
- `LICENSE` — лицензия MIT с указанием copyright 2026 Li_is.

## Ключевые методы/функции

Не применимо: корневые файлы не объявляют методы.

## Зависимости

- Использует: нет.
- Используется в: [[Architecture]].

## Важные детали

Агент реализован отдельным crate в `Lognara-agent/`; в корне общего Cargo workspace нет.
