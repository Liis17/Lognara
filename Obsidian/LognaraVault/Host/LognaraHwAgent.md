# lognara-hwagent

Parent: [[Index]]

## Назначение и границы

Агент Linux-хоста (`Lognara-hwagent/`), работает на машине под systemd, а не в контейнере. Собирает метрики машины, процессы и системные логи и отправляет их прямо в core без relay. Нужен для расследования взлома: видно, что происходило на хосте в момент времени, а на метрики и логи ставятся алерты Dock. Установка, конфигурация и ограничения: `Lognara-hwagent/README.md`.

Источник всех событий фиксирован: `service=host`, `backend=host`, `server` - имя хоста (`LOGNARA_SERVER` или `/proc/sys/kernel/hostname`). Узел хоста в core описан в разделе «Хосты» [[Core/LognaraCore]].

## Поток и доставка

Сборщики → `Collector::save` → durable очередь агента ([[Agent/LognaraAgent]], раздел «Встраивание») → `CoreEncoder` (`src/encoder.rs`) → `POST /v1/batches` core с токеном хоста. Кодировщик разбирает пачку агента типами relay (`agent_wire`), нормализует записи `normalize::event_with_budget` и кодирует `CoreBatch` одной группой, подписанной SHA-256 токена. Так тело строится один раз до первой попытки: UUID событий фиксированы, повтор идёт теми же байтами, core дедуплицирует его. Запись, не помещающаяся в бюджет нормализации, заменяется короткой отметкой, чтобы очередь не останавливалась. Для нормализации в процессе hwagent использует резерв 128 MiB, пачку 4 MiB и запись до 512 KiB; тело до 16 MiB. Heartbeat (`src/heartbeat.rs`) идёт на `/v1/heartbeat` в формате relay с одной записью агента хоста.

Решение отправлять без relay принято пользователем: hwagent должен работать без relay, а relay требует минимум 2048 MiB RAM. Из relay используются только библиотечные модули нормализации и формата core.

## Сборщики

- **Метрики** (`src/metrics.rs`): `/proc` и `/sys` - CPU (usage, iowait, steal), loadavg, память и swap, свободное место ФС, байты дисков и сети, PSI; GPU AMD через sysfs и NVIDIA через `nvidia-smi` с таймаутом. Запись `ServiceMetrics` SchemaVersion 2 в attributes `Metrics`: core извлекает точки без правил ([[Design/Metrics]]). Имена плоские; виртуальные интерфейсы и ФС контейнеров исключены, устройств каждого вида не больше 16. При полной очереди отчёт пропускается.
- **Процессы** (`src/processes.rs`): опрос `/proc`. События `process.new_executable` (ключ - контейнер и путь exe, множество известных файлов в state-файле до 20 000), `process.suspicious` (exe удалён или в `/tmp`, `/var/tmp`, `/dev/shm`), по опции `process.start`, раз в минуту `process.top` с метриками `proc.count` и `proc.top_cpu_percent`. Первый запуск запоминает файлы без событий. Известные файлы сохраняются только после записи событий опроса в spool; если очередь не приняла события, процессы сообщаются снова на следующем опросе. Секреты в cmdline маскируются. Короткоживущие процессы опрос не видит; полный аудит exec - лог auditd.
- **Журнал** (`src/journald.rs`): `journalctl --follow --output=json`; cursor сохраняется в state только после durable-записи. Первый запуск читает новые записи и один раз догружает историю auth/authpriv (входы, sudo). При полной очереди чтение pipe ждёт.
- **Файлы**: секция `sources` в формате источников агента ([[Agent/LogSources]]). Примеры `examples/hwagent.json` (journald плюс audit, fail2ban, история пакетов) и `examples/hwagent-rsyslog.json` (только файлы); journald и файлы rsyslog одновременно дали бы дубли.

## Проверка

Unit-тесты на фикстурах `/proc` и `/sys`, разбор journald и cursor, маскирование cmdline; `tests/e2e.rs` с фейковым core проверяет файл → нормализацию → доставку, точный повтор после 503, подпись группы и heartbeat. Static musl-сборка проверяется `.github/workflows/build-hwagent.yml` ([[Repository/CI]]); архивы из `master` публикуются в GitHub Releases ([[Repository/Images#Архивы хост-агента]]).
