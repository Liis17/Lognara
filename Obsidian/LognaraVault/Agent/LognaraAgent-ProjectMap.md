# LognaraAgent - Project Map

Parent: [[Agent/LognaraAgent]]

| Файл (в Lognara-agent/) | Символы | Назначение |
|---|---|---|
| `src/main.rs` | `main`, `start` | CLI, env/JSON, неблокирующая диагностика, bind до запуска. |
| `src/lib.rs` | `run`, `Runtime` | HTTP/файлы/sender и порядок shutdown. |
| `src/config.rs` | `Config`, `load_sources`, `validate` | Общая конфигурация, memory budget. |
| `src/sources.rs` | `Sources`, `Source`, `Multiline` | Строгая схема и bounded настройки. |
| `src/framing.rs` | `Framer`, `Framed`, `Continuation` | Text/NDJSON/multiline, фрагментация и byte ranges. |
| `src/files.rs` | `Files`, `Checkpoint`, `Position` | Discovery, fair read, fingerprint, durable позиции и ротация. |
| `src/process.rs` | `run_program`, `Capture`, `Losses`, `Signals` | Child process, pipe readers, очереди разбора, сигналы и потери. |
| `src/output.rs` | `Console` | Независимые bounded std writers echo/диагностики. |
| `src/ingest.rs` | `router`, `parse_body_limited` | Совместимый HTTP приём. |
| `src/buffer.rs` | `Buffer`, `SourceAdmission` | Единая durable очередь и memory admission всех источников. |
| `src/wire.rs` | `Batch`, `Record`, `Origin` | Совместимый MessagePack и provenance. |
| `src/heartbeat.rs` | `Telemetry`, `run` | Отдельный JSON heartbeat в relay, возраст отметок, версия и spool. |
| `src/sender.rs` | `Sender`, `Relay` | Подготовка и повтор сохранённых тел. |
| `tests/e2e.rs`, `sources.rs`, `framing.rs`, `files.rs`, `process.rs` | интеграционные тесты | HTTP ACK, форматы, crash/restart, ротация, лимиты, жизненный цикл. |
| `tests/memory_profile.rs`, `check-memory-profile.py` | `bounded_agent_memory_profile`, `bounded_source_memory_profile` | RSS при конкурентном HTTP, outage и восстановлении; source case включает files/stdout/stderr. |
| `docs/log-sources.md`, `examples/log-sources/` | руководство, JSON | Подключение программ и эксплуатационные гарантии. |

Связи: источники → Framer → Buffer → Sender → relay normalize → core attributes. Checkpoint и spool имеют одну блокировку каталога. Служебные атрибуты описаны в [[Agent/LogSources]].
