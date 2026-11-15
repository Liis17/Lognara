# lognara-hwagent

Агент Linux-хоста. Работает на самой машине (systemd, не в Docker) и отправляет данные прямо в lognara-core, без relay:

- **метрики** CPU, памяти, swap, файловых систем, дисков, сети, PSI и GPU (AMD через sysfs, NVIDIA через `nvidia-smi`) во встроенном формате `ServiceMetrics`: графики и алерты метрик Dock работают без правил;
- **процессы**: новые исполняемые файлы, подозрительные процессы (exe удалён или лежит в `/tmp`, `/var/tmp`, `/dev/shm`), top-N по CPU и памяти раз в минуту;
- **логи** системного журнала (journald) и файлов из конфигурации: входы, sudo, ядро, audit, fail2ban, установка пакетов.

Все события приходят с источником `service=host`, `backend=host`, `server=<имя хоста>` и узлом хоста (`relay_id`/`agent_id`), поэтому их можно искать, ограничивать скоупом токена и ставить на них алерты.

## Установка

1. В Dock откройте «Доступ → Хост-агенты» и создайте хост. Токен показывается один раз.
2. Скачайте из GitHub Releases релиз с тегом `hwagent-*` (не `agent-*`) и архив `lognara-hwagent-linux-amd64.tar.gz` или `-arm64`, проверьте сумму по `SHA256SUMS`, распакуйте и скопируйте бинарник `lognara-hwagent` (static musl) в `/usr/local/bin/`. Пример env, unit и конфигурации лежат в архиве в `packaging/` и `examples/`.
3. Создайте `/etc/lognara/hwagent.env` по [`packaging/hwagent.env.example`](packaging/hwagent.env.example) с правами `0600 root:root`.
4. Скопируйте конфигурацию в `/etc/lognara/hwagent.json`:
   - [`examples/hwagent.json`](examples/hwagent.json): система с journald (Debian 12+, Ubuntu, RHEL). Журнал даёт sshd, sudo, ядро, cron и юниты; файлы добавляют audit, fail2ban и историю пакетов.
   - [`examples/hwagent-rsyslog.json`](examples/hwagent-rsyslog.json): система, где журнал пишется rsyslog в `/var/log`. journald выключен, иначе события sshd и sudo придут дважды.
5. Установите [`packaging/lognara-hwagent.service`](packaging/lognara-hwagent.service) в `/etc/systemd/system/` и запустите `systemctl enable --now lognara-hwagent`.

После утечки токена смените его в Dock («Сменить токен») и перезапустите службу: история и привязки скоупов сохраняются, старый токен перестаёт работать сразу.

## Переменные окружения

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_CORE_URL` | да | - | `https://core/v1/batches`; heartbeat идёт на `/v1/heartbeat` |
| `LOGNARA_HOST_TOKEN` | да | - | токен хоста из Dock |
| `LOGNARA_SERVER` | нет | hostname | поле `server` |
| `LOGNARA_ENVIRONMENT` | нет | нет | поле `environment` |
| `LOGNARA_HWAGENT_CONFIG` | нет | `/etc/lognara/hwagent.json` | JSON-конфигурация |
| `LOGNARA_SPOOL_DIR` | нет | `/var/lib/lognara-hwagent/spool` | durable-очередь |
| `LOGNARA_STATE_DIR` | нет | `/var/lib/lognara-hwagent/state` | cursor журнала и известные исполняемые файлы |
| `LOGNARA_SPOOL_MAX_MB`, `LOGNARA_FLUSH_INTERVAL_MS`, `LOGNARA_BATCH_SIZE`, `LOGNARA_MAX_BUFFER`, `LOGNARA_HEARTBEAT_INTERVAL_MS` | нет | как у агента | очередь и отправка |

## Конфигурация

Неизвестные поля - ошибка запуска.

- `sources`: файлы логов в формате источников агента ([документация](../Lognara-agent/docs/log-sources.md)): checkpoint, ротация, multiline. Только `type: file`.
- `journald`: `enabled`, `journalctl` (абсолютный путь), `matches` (совпадения journalctl, пусто - весь журнал), `backfill_auth` (при первом запуске догрузить историю auth/authpriv).
- `metrics`: `enabled`, `interval_seconds` (5..3600), `exclude_interfaces` (префиксы), `nvidia_smi` (пусто - без NVIDIA).
- `processes`: `enabled`, `interval_seconds`, `top`, `top_interval_seconds`, `report_starts` (событие на каждый новый процесс), `cmdline` (`false` - не отправлять аргументы), `suspicious_dirs`.

## Гарантии и ограничения

- Пачка нормализуется по правилам relay и сохраняется в `pending/` до первой попытки; повторы идут теми же байтами, core дедуплицирует их. Подтверждение core означает fsync WAL.
- Cursor журнала и позиции файлов сохраняются после durable-записи: падение даёт повтор, а не пропуск.
- Догрузка истории auth/authpriv при первом запуске идёт параллельно с новыми записями и не гарантирует всю историю: падение в её середине не продолжит её после рестарта, а рестарт до первой записи cursor повторит её с новыми ID событий.
- При полной очереди события процессов не теряются: процессы с событиями сообщаются снова на следующем опросе, известные исполняемые файлы сохраняются только после записи событий.
- При полной очереди отчёт метрик пропускается, журнал и файлы ждут.
- Опрос процессов раз в несколько секунд не видит короткоживущие процессы. Для полного аудита exec включите auditd: его лог уже есть в примерах.
- Первый запуск запоминает текущие исполняемые файлы без событий; подозрительные процессы сообщаются сразу. Известные файлы ограничены 20 000.
- Аргументы процессов, похожие на пароли и токены (`--password=`, `token=`, `Authorization:`, `-u user:pass`, `-pPASS`, пароль в URL), заменяются на `***`; длина ограничена 1024 байтами.
- Имена метрик плоские (`net.eth0.rx_bytes`, корневая ФС - `fs.rootfs.*`); виртуальные интерфейсы и ФС контейнеров исключены, устройств каждого вида не больше 16.
- Убитый агент молчит. Поставьте в Dock алерт на `cpu.usage_percent` хоста с режимом потери данных: он сообщит, что хост перестал присылать метрики.
- Злоумышленник с root может подменить локальные логи, но уже отправленные в core события остаются неизменными.
