# Корневые файлы

Parent: [[Index]]

## Назначение

Описывает корневые файлы репозитория Lognara.

## Файлы

- `README.md` — описание проекта на русском: назначение, схема потока логов (Mermaid), возможности компонентов, быстрый старт, порты и параметры `LOGNARA_*`, общий ключ agent → relay, `.env` и фрагмент Compose с выделенной сетью, durable ACK и обязательное хранилище agent, relay и core (папка на хосте или Docker volume), порядок обновления, HTTP API core, эксплуатация, разработка, структура репозитория, статус и ограничения. Источники и запуск программ описаны в `Lognara-agent/docs/log-sources.md`, примеры в `Lognara-agent/examples/log-sources/`. Детали core вынесены в `Lognara-core/README.md` и `Lognara-core/docs/`.
- `.gitignore` — содержит исключения для Cargo, артефактов сборки агента в `/build/`, rustfmt, PDB, Cargo Mutants, RustRover, `default.profraw`, `.DS_Store`, локального `.env` с ключом, а также `node_modules/` и `Lognara-dock/web/dist/`.
- `.env.example` — пустой обязательный `LOGNARA_RELAY_TOKEN` и инструкция сгенерировать ключ через `openssl rand -hex 32`, токены core `LOGNARA_CORE_TOKEN` и `LOGNARA_CORE_QUERY_TOKEN`, переменные хранения `LOGNARA_*_DATA` (пусто = volume, абсолютный путь = папка на хосте), `LOGNARA_QUERY_TOKEN` для dock (равен `LOGNARA_CORE_QUERY_TOKEN` при совместном использовании примеров); копируется в `.env` для настройки Compose.
- `LICENSE` — лицензия MIT с указанием copyright 2026 Li_is.
- `.github/workflows/ci.yml` — GitHub Actions для всех веток и PR: сборка и тесты пяти Rust-crate, Clippy, форматирование, проверки памяти agent/relay и сборка веб-интерфейса. Детали в [[Repository/CI]].
- `.dockerignore` — исключает git, локальные Rust/Node-сборки и `.env` из корневого контекста Docker-сборки core и relay. Поставка в [[Repository/Images]].

`.env.example` также задаёт `LOGNARA_IMAGE_TAG=master` для согласованного выбора тегов core, relay и dock в примерах Compose; можно заменить на `sha-<полный SHA коммита>`.

## Ключевые методы/функции

Не применимо: корневые файлы не объявляют методы.

## Зависимости

- Использует: нет.
- Используется в: [[Architecture]].

## Важные детали

Агент реализован отдельным crate в `Lognara-agent/`; в корне общего Cargo workspace нет.
