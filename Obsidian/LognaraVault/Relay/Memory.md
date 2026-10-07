# Relay memory

Parent: [[Relay/LognaraRelay]]

## Admission и конфигурация

`memory.rs` разделяет резервы приёма, накопленных моделей и отправки. Конфигурация проверяет сумму `ingest_request_bytes * concurrency + max_buffer_bytes + sender_bytes` без переполнения. По умолчанию это 1024 + 256 + 512 = 1792 MiB; профиль рассчитан на процесс с 2 GiB, оставляя 256 MiB для runtime, транспорта и allocator.

Входной резерв: два сжатых тела по 64 MiB, буфер распаковки 256 MiB + 1, бюджет модели, 128 MiB для codec и 128 MiB запаса; сумма округляется вверх до 128 MiB. Резерв sender: `core_max_decoded_bytes + 2 * core_max_body_bytes + 128 MiB`.

После авторизации и проверки заголовков `ingest` без ожидания резервирует slot. Без него отвечает `503` и не читает тело. Чтение ограничено 64 MiB и 30 секундами. Permit переносится в blocking-задачу: отмена ожидающего HTTP-запроса не освобождает память работающего декодера. При остановке `Resources::drain` ждёт завершения всех допущенных работников до остановки sender.

| Метод | Назначение |
|---|---|
| `Resources::new(config: &Config): Arc<Resources>` | Создаёт slots и пул накопленных моделей. |
| `Resources::ready(): bool` | Проверяет доступность приёма. |
| `Resources::set_ready(ready: bool)` | Меняет доступность приёма. |
| `Resources::drain()` | Закрывает admission и ждёт работников. |
| `run_blocking(permit: OwnedSemaphorePermit, work: FnOnce): Result<T, JoinError>` | Удерживает permit до завершения blocking-работы. |
| `Pool::reserve(bytes: usize): Result<Reservation, ReserveError>` | Без ожидания резервирует байты; отличает TooLarge от Full. |
| `Reservation::drop()` | Возвращает резерв в пул. |
| `Config::validate_memory(): Result<(), ConfigError>` | Проверяет ресурсную конфигурацию, включая программно созданную. |
| `Config::ingest_request_bytes(): Option<usize>` | Считает фиксированный резерв приёма. |
| `Config::sender_bytes(): Option<usize>` | Считает отдельный резерв отправки. |

Бюджет ограничивает управляемые аллокации, а не гарантирует точный RSS. Профиль проверяется нагрузочным тестом.
