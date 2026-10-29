# lognara-dock

Веб-интерфейс Lognara. Отдаёт React-приложение, проверяет вход по htpasswd/passkey, показывает историю логов, связанные логи Trace, отдельный Live и базовую аналитику core. Работает за nginx, который завершает TLS.

Самостоятельный crate, Rust 2024, минимальный Rust 1.94. Фронтенд находится в `web/` (React, TypeScript, Vite).

```
Браузер ──TLS──> nginx :443 ──http──> lognara-dock :7403 ──Bearer query──> lognara-core :7402
                                       ├─ /etc/lognara-dock/auth/htpasswd   (папка с хоста)
                                       └─ /var/lib/lognara-dock/dock.sqlite3 (папка с хоста)
```

Query- и admin-токены core хранятся только в dock и не передаются в браузер.

## Вход

**htpasswd.** Пользователей задаёт файл в формате docker registry: строки `user:bcrypt-hash`. Поддерживается только bcrypt (`$2y$`, `$2b$`, `$2a$`). Строка с MD5, SHA или crypt останавливает запуск, ошибка называет номер строки. Пустые строки и строки с `#` пропускаются.

```sh
mkdir -p auth data
htpasswd -B -C 12 -c auth/htpasswd liis
# без apache2-utils:
docker run --rm httpd:2.4 htpasswd -Bbn liis 'пароль' >> auth/htpasswd
```

- Изменения файла с хоста применяются без рестарта: dock перечитывает его при изменении.
- Пользователь, удалённый из файла, сразу теряет доступ, в том числе по уже открытой сессии и по passkey.
- Если файл стал некорректным, вход запрещён, а `/health/ready` отвечает `503`, пока файл не исправят.
- Смена пароля в настройках атомарно заменяет только строку пользователя (временный файл, fsync, rename). Остальные сессии пользователя при этом завершаются.

> ⚠️ Монтируйте **папку** `./auth`, а не сам файл. `rename` поверх bind-mount одиночного файла даёт `EBUSY`, и смена пароля не сработает. Папка должна быть доступна на запись пользователю контейнера (по умолчанию `1000:1000`).

**Passkey.** Добавляется в настройках: нужны имя ключа и текущий пароль. На странице входа кнопка «Войти по passkey» работает без ввода логина: браузер предлагает ключи этого сайта. Вход по паролю остаётся доступен всегда.

- RP ID равен хосту `LOGNARA_DOCK_PUBLIC_URL`. Если сменить домен, ранее созданные passkey перестанут подходить.
- Удаление ключа тоже требует текущий пароль.

**Сессии.** Cookie `lognara_dock_session` с флагами `HttpOnly; Secure; SameSite=Strict`. В SQLite хранится только SHA-256 токена. Сессия ограничена абсолютным сроком и временем бездействия. POST и DELETE принимаются, только если заголовок `Origin` совпадает с `LOGNARA_DOCK_PUBLIC_URL`. После 5 неверных паролей подряд вход по паролю для этого логина блокируется на 5 минут (`429`), вход по passkey при этом работает.

## Логи в реальном времени

Страница `/logs` открывает исторический поиск: строка запроса, период, 50 событий на страницу и «Загрузить ещё». Поддерживаются точные поля (`service`, `level`, source/trace/request IDs), AND между условиями, OR внутри значений одного поля и text all/phrase. Пример: `service:billing AND level:(ERROR OR WARN)`. Запрос, from/to и выделенное событие сохраняются в URL. Раскрытие строки показывает поля/JSON, контекст источника ±5 минут, связанные логи Trace и ссылку. Экспорт JSON/CSV выгружает только уже загруженные строки; CSV экранирует формулы, JSON сохраняет значения.

Live включается отдельной кнопкой. История сохраняет порядок core и повторяющиеся id; Live использует прежнюю дедупликацию по id и ограниченный буфер. Trace показывает связанные события по времени ASC, без выдуманных spans/длительностей.

Один общий опрос на процесс работает, пока открыта хотя бы одна страница логов. Раз в `LOGNARA_DOCK_LIVE_POLL_MS` dock запрашивает `POST /v1/logs/search` за окно `[now - LOOKBACK, now + 60s)` с `limit=1000` и отправляет в браузер только новые id через SSE (`GET /api/logs/stream`). Новый подписчик сначала получает до 200 последних событий. Браузер держит последние 2000 строк.

Окно назад нужно, потому что relay по умолчанию отправляет пачки раз в 60 секунд. Событие, которое пришло позже окна, в ленту не попадёт.

**Ограничение.** Core ищет только по времени события. Если за один опрос в окне появилось больше 1000 новых событий, лента покажет самые свежие 1000. Полное решение требует tail-endpoint в core по порядку публикации.

## Базовая аналитика

`/analytics`: периоды 1 час/24 часа/7 дней, сервис, общий count, средняя частота за окно, доля ERROR, все семь уровней и до 5 сервисов по ERROR. Сравнение использует предыдущее окно той же длины. Выбор корзины, уровня или сервиса открывает соответствующие логи. Список сервисов ограничен 100 активными значениями; каждый блок показывает свой `as_of`, снимки независимы.

Текущая аналитика core не принимает текст поиска: `/logs` для text-запроса показывает только загруженные строки и объясняет недоступность общего count/гистограммы. P95, паттерны, проблемы, инфраструктурная телеметрия, алерты Telegram/Discord и spans относятся к следующим этапам [плана](../docs/DockDesignIntegrationPlan.md).

## Подключения relay и агентов

После запуска core и dock войдите и откройте `/access` («Подключения»). Формы создания relay и агента находятся вверху страницы; после создания relay он выбирается для следующего агента. Агенты показаны плитками внутри своего relay. Dock показывает токен только после создания или перевыпуска и готовую строку `.env`: `LOGNARA_RELAY_CORE_TOKEN=lnr_…` либо `LOGNARA_AGENT_TOKEN=lna_…`. Поле допускает выделение фрагментов; кнопки копируют всю строку или только токен. Сохраните секрет сразу: повторно показать его нельзя. До «Я сохранил токен» выдача следующего секрета заблокирована. Токены передаются процессам как `LOGNARA_CORE_TOKEN` relay и `LOGNARA_RELAY_TOKEN` агента.

«Перевыпустить токен» на плитке агента требует подтверждения. Меняется только токен; ID, имя, relay, дата создания и heartbeat сохраняются. Установите новый токен в конфигурации агента. Старый перестаёт работать после следующей успешной синхронизации relay с core, обычно в пределах 10 секунд; при недоступном core отзыв задерживается. Если ответ перевыпуска потерялся, запрос автоматически не повторяется: можно явно перевыпустить токен снова.

Реестр и SHA-256-хеши хранятся в core. Списки не содержат токенов. Перед удалением relay удалите его агентов. Core сразу отзывает relay-токен; relay применяет изменение списка агентов при следующем успешном обновлении (каждые 10 секунд). При недоступном core продолжает работать по кешу. Имя агента уникально внутри relay; метаданные источника не проверяются.

API dock: GET/POST `/api/relays`, `/api/agents`, DELETE `/api/relays/{id}`, `/api/agents/{id}`. POST `/api/agents/{id}/rotate-token` без тела возвращает `200 {"agent":{…},"token":"lna_…"}` с `Cache-Control: no-store`; неизвестный агент — `404 not_found`. Требуется сессия, изменения требуют корректный Origin. Конфликты core возвращаются как 409, недоступность core — 502. Admin-токен core в браузер не попадает.

## Параметры

Все параметры задаются переменными окружения и проверяются при старте. При ошибке процесс завершается с кодом 1 и называет переменную.

| Переменная | По умолчанию | Назначение |
|---|---|---|
| `LOGNARA_DOCK_PUBLIC_URL` | обязательна | внешний адрес, например `https://dock.liis17.com`. Используется как origin для passkey и проверки `Origin`. `http` допускается только для `localhost` |
| `LOGNARA_DOCK_CORE_URL` | обязательна | базовый адрес core, например `http://lognara-core:7402` |
| `LOGNARA_DOCK_CORE_TOKEN` | обязательна | равен `LOGNARA_QUERY_TOKEN` core, ASCII без пробелов |
| `LOGNARA_DOCK_CORE_ADMIN_TOKEN` | обязательна | равен `LOGNARA_ADMIN_TOKEN` core, ASCII без пробелов, отличается от query; нужен для relay, агентов и определений метрик |
| `LOGNARA_DOCK_LISTEN_ADDR` | `127.0.0.1:7403` | в контейнере `0.0.0.0:7403` (задано в образе) |
| `LOGNARA_DOCK_HTPASSWD_FILE` | `/etc/lognara-dock/auth/htpasswd` | файл пользователей внутри смонтированной папки |
| `LOGNARA_DOCK_DATA_DIR` | `/var/lib/lognara-dock` | SQLite с сессиями и passkey, lock-файл |
| `LOGNARA_DOCK_RP_ID` | хост из `PUBLIC_URL` | RP ID для passkey: хост или его родительский домен |
| `LOGNARA_DOCK_SESSION_TTL_SECONDS` | `43200` | абсолютный срок сессии |
| `LOGNARA_DOCK_SESSION_IDLE_SECONDS` | `3600` | завершение сессии при бездействии |
| `LOGNARA_DOCK_BCRYPT_COST` | `12` | cost для нового пароля, от 10 до 14 |
| `LOGNARA_DOCK_PASSWORD_MIN_LENGTH` | `12` | минимальная длина нового пароля в символах (максимум 71 байт) |
| `LOGNARA_DOCK_LIVE_POLL_MS` | `1000` | частота опроса core |
| `LOGNARA_DOCK_LIVE_LOOKBACK_SECONDS` | `300` | окно для поздно доставленных событий |
| `LOGNARA_DOCK_WEB_DIR` | `/usr/share/lognara-dock/web` | собранный фронтенд, в образе уже есть |
| `RUST_LOG` | `info` | уровень логов |

Каталог данных блокируется одним процессом. Для резервной копии остановите dock и скопируйте `data/` целиком.

## Docker и nginx

- [`Dockerfile`](Dockerfile) собирает фронтенд и бинарник. В образе процесс работает от `1000:1000`.
- [`docker-compose.example.yml`](docker-compose.example.yml) использует `docker.barkfluff.com/lognara-dock:${LOGNARA_IMAGE_TAG:-master}`, содержит все переменные, тома `./auth` и `./data`, публикует порт только на `127.0.0.1`.
- [`nginx.example.conf`](nginx.example.conf) показывает TLS и отдельный `location` для SSE без буферизации.

nginx должен передавать заголовок `Origin` без изменений.

```sh
cd Lognara-dock
cp docker-compose.example.yml docker-compose.yml   # поправьте домен и адрес core
echo "LOGNARA_QUERY_TOKEN=<query-токен core>" > .env
docker compose pull
docker compose up -d
```

Образ публикуется после успешного CI в `master` при изменениях dock или при ручном `Release all containers`, для `linux/amd64`. Перед публикацией версия dock считывается из registry и увеличивается на один patch независимо от core и relay, начиная с `0.0.1`. Для фиксации сборки задайте в `.env` `LOGNARA_IMAGE_TAG=0.0.1` или `sha-<полный SHA коммита>`; общий SHA для всех трёх образов появляется после полного релиза. До первой публикации или для локальной сборки замените `image` на `build: { context: .., dockerfile: Lognara-dock/Dockerfile }` и запускайте `docker compose up -d --build`. Перед скачиванием приватного образа выполните `docker login docker.barkfluff.com`. Общая схема описана в [корневом README](../README.md#обновление).

## HTTP API

| Метод и путь | Доступ | Назначение |
|---|---|---|
| `GET /health/live`, `/health/ready` | открыт | живость; ready: htpasswd прочитан и корректен |
| `POST /api/auth/login` | открыт | `{username, password}`, ставит cookie сессии |
| `POST /api/auth/passkey/start`, `/finish` | открыт | вход по passkey |
| `POST /api/auth/logout` | сессия | завершить сессию |
| `GET /api/me` | сессия | `{username, passkeys}` |
| `POST /api/account/password` | сессия | `{current, new}` |
| `GET /api/account/passkeys` | сессия | список ключей |
| `POST /api/account/passkeys/start` | сессия | `{name, password}`, начать регистрацию |
| `POST /api/account/passkeys/finish` | сессия | завершить регистрацию |
| `DELETE /api/account/passkeys/{id}` | сессия | `{password}`, удалить ключ |
| `GET /api/infrastructure` | сессия | core/dock/узлы/storage, freshness и пороги; частичные ошибки явны |
| `GET /api/logs/stream` | сессия | SSE: `logs`, `core_error`, `core_ok`, `lagged` |
| `POST /api/logs/search` | сессия + Origin | история: from/to, filters, text, limit, cursor; ответ events/next_cursor/as_of |
| `GET /api/traces/{trace_id}` | сессия | связанные логи ASC; from/to/limit/cursor в query |
| `POST /api/stats/histogram` | сессия + Origin | from/to/filters/interval_seconds; buckets count и as_of |
| `POST /api/stats/group-by` | сессия + Origin | from/to/filters/group_by/limit; groups count и as_of |
| `POST /api/stats/summary` | сессия + Origin | from/to/filters/text/interval_seconds/search_limit; согласованные total/buckets/levels/error_services/duration/actions/page/as_of |
| `GET /api/series` | сессия | список метрик core (`GET /v1/series`) |
| `GET /api/series?group_by=server` | сессия | метрики по серверам; необязательные фильтры `server`, `service` |
| `POST /api/series/lookup` | сессия + Origin | метаданные выбранных ключей `{keys:[{server,service,name,origin}]}` |
| `GET /api/series/favorites` | сессия | общее избранное `{keys:[...]}`, хранится в Dock |
| `PUT`, `DELETE /api/series/favorites` | сессия + Origin | идемпотентное добавление/удаление одного ключа, ответ 204 |
| `GET /api/series/sources` | сессия | сохранённые пары сервер/сервис, heartbeat и определения без точек |
| `POST /api/series/import/check`, `/apply` | сессия + Origin, admin | проверка переносимого набора, разрешение конфликтов и атомарное сохранение |
| `POST /api/series/query` | сессия | значения серии по корзинам (`POST /v1/series/query`), таймаут 35 с |
| `GET`, `POST /api/series/definitions` | сессия, admin | список и создание определений |
| `PUT`, `DELETE /api/series/definitions/{id}` | сессия, admin | замена и удаление определения |
| `POST /api/series/preview` | сессия, admin | предпросмотр правила на логах core |
| `GET /*` | открыт | SPA, неизвестный путь отдаёт `index.html` |

Маршруты с пометкой admin dock вызывает в core с `LOGNARA_DOCK_CORE_ADMIN_TOKEN`. Тело запроса к core передаётся как есть, проверяет его core.

Ошибки возвращаются как JSON `{"error": "<код>"}`: `unauthorized`, `forbidden`, `too_many_attempts`, `invalid_current_password`, `password_too_short`, `password_too_long`, `invalid_name`, `challenge_expired`, `passkey_rejected`, `passkey_exists`, `not_found`. Для метрик добавлены `core_unavailable` (502: core недоступен, не ответил вовремя или вернул 5xx) и `invalid` (400: ответ содержит `detail` с причиной от core).

Страница событий получает первую страницу, общий подсчёт и stacked-график одним summary, включая текстовые фильтры. Аналитика использует одну сводку на окно, показывает приблизительный p95 `duration_ms`, покрытие измерений и до 20 action-групп по числу событий. Прошлое окно получает отдельную сводку. [Контракт summary](../docs/DockSummaryContract.md).

Query API сохраняет безопасную классификацию ошибок: `invalid_request`/`invalid_query` (400), `cursor_expired` (410), `query_unavailable` (503), `query_timeout` (504); недоступный/неожиданный core — `core_unavailable` (502). Query timeout 35 секунд, Live/admin 10 секунд. Все ответы `/api/*` имеют `Cache-Control: no-store`.

## Инфраструктура

`/infrastructure` показывает цепочку, зарегистрированные relay/агенты, heartbeat/версию/spool/последнее событие, lag публикации и WAL core, SSE/сессии Dock и текущее место на filesystem core. `GET /api/infrastructure` требует сессию, no-store; query/admin секреты остаются в Dock. Опрос через 10 секунд после завершения предыдущего запроса; ошибка сохраняет явно устаревший снимок.

У agent/relay `LOGNARA_HEARTBEAT_INTERVAL_MS` default 10000, диапазон 1..30000 мс. TTL 60 с, idle 300 с, spool degraded от 80%; до первого heartbeat «Нет данных», молчащий relay не доказывает отключение агента. История, per-node dropped, p95 доставки и прогноз диска не реализованы; source-ссылки строят фильтр по конфигурации, не проверяют agent_id событий. [Полный контракт](../docs/DockInfrastructureContract.md).

## Разработка

```sh
cargo test   --manifest-path Lognara-dock/Cargo.toml
cargo fmt    --manifest-path Lognara-dock/Cargo.toml --check
cargo clippy --manifest-path Lognara-dock/Cargo.toml --all-targets -- -D warnings

cd Lognara-dock/web && npm ci && npm run build
npm test
npx playwright install chromium
npm run test:e2e
```

Для сборки на хосте нужен OpenSSL (зависимость webauthn-rs). Тесты поднимают поддельный core и проходят полную церемонию passkey с программным аутентификатором.

Локальный запуск после `npm run build`:

```sh
LOGNARA_DOCK_PUBLIC_URL=http://localhost:7403 \
LOGNARA_DOCK_CORE_URL=http://127.0.0.1:7402 \
LOGNARA_DOCK_CORE_TOKEN=query-secret \
LOGNARA_DOCK_HTPASSWD_FILE=/tmp/lognara-demo/auth/htpasswd \
LOGNARA_DOCK_DATA_DIR=/tmp/lognara-demo/dock \
LOGNARA_DOCK_WEB_DIR=Lognara-dock/web/dist \
Lognara-dock/target/debug/lognara-dock
```

Для работы с Vite (`npm run dev`, порт 5173) запустите dock с `LOGNARA_DOCK_PUBLIC_URL=http://localhost:5173`: Vite проксирует `/api` на `127.0.0.1:7403`, а `Origin` и passkey проверяются по адресу, открытому в браузере.

## Метрики по серверам

Раздел `/metrics` сначала загружает только общее избранное, сгруппированное по серверам и сервисам. Остальные метрики закрыты; список источников загружается после раскрытия раздела, каталог сервиса - после раскрытия сервера и сервиса. Звёздочка карточки добавляет/удаляет избранное без перехода, доступна при наведении, фокусе и на сенсорных устройствах. Избранное хранится в SQLite Dock и сохраняется после перезапуска. Плитка открывает отдельную страницу с графиком и настройками этого источника. Создание доступно через модалку: ручное правило, JSON-набор или шаблоны nginx. Сначала выберите сервер, затем сервис.

[Контракт метрик и миграция Core](../docs/MetricsBundle.md), [инструкция для генерации JSON другим агентом](web/public/metrics-guide.md), [JSON Schema](web/public/metrics-schema.json). Статические файлы доступны в Dock по `/metrics-guide.md`, `/metrics-schema.json`, `/metrics-example.json`, `/metrics-nginx.json`.
