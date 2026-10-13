# lognara-dock

Веб-интерфейс Lognara. Отдаёт React-приложение, проверяет вход по htpasswd или passkey и показывает логи core в реальном времени. Работает за nginx, который завершает TLS.

Самостоятельный crate, Rust 2024, минимальный Rust 1.94. Фронтенд находится в `web/` (React, TypeScript, Vite).

```
Браузер ──TLS──> nginx :443 ──http──> lognara-dock :7403 ──Bearer query──> lognara-core :7402
                                       ├─ /etc/lognara-dock/auth/htpasswd   (папка с хоста)
                                       └─ /var/lib/lognara-dock/dock.sqlite3 (папка с хоста)
```

Query-токен и admin-токен core хранятся только в dock и не передаются в браузер.

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

Один общий опрос на процесс работает, пока открыта хотя бы одна страница логов. Раз в `LOGNARA_DOCK_LIVE_POLL_MS` dock запрашивает `POST /v1/logs/search` за окно `[now - LOOKBACK, now + 60s)` с `limit=1000` и отправляет в браузер только новые id через SSE (`GET /api/logs/stream`). Новый подписчик сначала получает до 200 последних событий. Браузер держит последние 2000 строк.

Окно назад нужно, потому что relay по умолчанию отправляет пачки раз в 60 секунд. Событие, которое пришло позже окна, в ленту не попадёт.

**Ограничение.** Core ищет только по времени события. Если за один опрос в окне появилось больше 1000 новых событий, лента покажет самые свежие 1000. Полное решение требует tail-endpoint в core по порядку публикации.

## Параметры

Все параметры задаются переменными окружения и проверяются при старте. При ошибке процесс завершается с кодом 1 и называет переменную.

| Переменная | По умолчанию | Назначение |
|---|---|---|
| `LOGNARA_DOCK_PUBLIC_URL` | обязательна | внешний адрес, например `https://dock.liis17.com`. Используется как origin для passkey и проверки `Origin`. `http` допускается только для `localhost` |
| `LOGNARA_DOCK_CORE_URL` | обязательна | базовый адрес core, например `http://lognara-core:7402` |
| `LOGNARA_DOCK_CORE_TOKEN` | обязательна | равен `LOGNARA_QUERY_TOKEN` core, ASCII без пробелов |
| `LOGNARA_DOCK_CORE_ADMIN_TOKEN` | не задан | равен `LOGNARA_ADMIN_TOKEN` core, ASCII без пробелов; нужен для определений метрик. Пустое значение отключает их: маршруты отвечают `404 admin_disabled`, в `/api/me` `series_admin` равен `false` |
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
- [`docker-compose.example.yml`](docker-compose.example.yml) содержит все переменные, тома `./auth` и `./data`, публикует порт только на `127.0.0.1`.
- [`nginx.example.conf`](nginx.example.conf) показывает TLS и отдельный `location` для SSE без буферизации.

nginx должен передавать заголовок `Origin` без изменений.

```sh
cd Lognara-dock
cp docker-compose.example.yml docker-compose.yml   # поправьте домен и адрес core
echo "LOGNARA_QUERY_TOKEN=<query-токен core>" > .env
# необязательно, для определений метрик: echo "LOGNARA_ADMIN_TOKEN=<admin-токен core>" >> .env
docker compose up -d --build
```

## HTTP API

| Метод и путь | Доступ | Назначение |
|---|---|---|
| `GET /health/live`, `/health/ready` | открыт | живость; ready: htpasswd прочитан и корректен |
| `POST /api/auth/login` | открыт | `{username, password}`, ставит cookie сессии |
| `POST /api/auth/passkey/start`, `/finish` | открыт | вход по passkey |
| `POST /api/auth/logout` | сессия | завершить сессию |
| `GET /api/me` | сессия | `{username, passkeys, series_admin}`; `series_admin` - задан ли admin-токен |
| `POST /api/account/password` | сессия | `{current, new}` |
| `GET /api/account/passkeys` | сессия | список ключей |
| `POST /api/account/passkeys/start` | сессия | `{name, password}`, начать регистрацию |
| `POST /api/account/passkeys/finish` | сессия | завершить регистрацию |
| `DELETE /api/account/passkeys/{id}` | сессия | `{password}`, удалить ключ |
| `GET /api/logs/stream` | сессия | SSE: `logs`, `core_error`, `core_ok`, `lagged` |
| `GET /api/series` | сессия | список метрик core (`GET /v1/series`) |
| `POST /api/series/query` | сессия | значения серии по корзинам (`POST /v1/series/query`), таймаут 35 с |
| `GET`, `POST /api/series/definitions` | сессия, admin | список и создание определений |
| `PUT`, `DELETE /api/series/definitions/{id}` | сессия, admin | замена и удаление определения |
| `POST /api/series/preview` | сессия, admin | предпросмотр правила на логах core |
| `GET /*` | открыт | SPA, неизвестный путь отдаёт `index.html` |

Маршруты с пометкой admin работают только при заданном `LOGNARA_DOCK_CORE_ADMIN_TOKEN`; иначе они отвечают `404` с кодом `admin_disabled`. Тело запроса к core передаётся как есть, проверяет его core.

Ошибки возвращаются как JSON `{"error": "<код>"}`: `unauthorized`, `forbidden`, `too_many_attempts`, `invalid_current_password`, `password_too_short`, `password_too_long`, `invalid_name`, `challenge_expired`, `passkey_rejected`, `passkey_exists`, `not_found`. Для метрик добавлены `core_unavailable` (503: core недоступен, не ответил вовремя или вернул 5xx), `invalid` (400: ответ содержит `detail` с причиной от core) и `admin_disabled` (404).

## Разработка

```sh
cargo test   --manifest-path Lognara-dock/Cargo.toml
cargo fmt    --manifest-path Lognara-dock/Cargo.toml --check
cargo clippy --manifest-path Lognara-dock/Cargo.toml --all-targets -- -D warnings

cd Lognara-dock/web && npm ci && npm run build
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
