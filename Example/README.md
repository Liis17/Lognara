# Локальный стенд Lognara

Четыре сервиса на одной машине:

- `app`: Python 3.13 HTTP-сервер на `8881` и собранный из исходников `lognara-agent` внутри контейнера. Приложение отправляет JSON-логи на `127.0.0.1:7400/v1/logs`.
- `relay`: отдельный `lognara-relay`, принимает пачки агента и отправляет их в core.
- `core`: отдельный `lognara-core`, хранит события и предоставляет поиск на `7402`.
- `dock`: `lognara-dock` с собранным React-интерфейсом, показывает логи в браузере на `7403`.

Служебный контейнер `dock-init` перед первым запуском создаёт bcrypt-пароль пользователя `demo` и права на volumes dock. После инициализации он завершается с кодом `0`; это штатное состояние.

Порты хоста привязаны к `127.0.0.1`; agent и relay наружу не публикуются. Сервисы общаются по внутренней Compose-сети. Admin/query-токены `local-admin-token` и `local-query-token` заданы в Compose для локального теста. Персональные токены relay и агента выдаёт core после регистрации через dock. Внешние базы данных не нужны.

## Запуск

Нужен Docker с Compose и Linux containers (на Windows можно использовать Docker Desktop). Из корня репозитория:

```sh
docker compose -f Example/compose.yaml config --quiet
docker compose -f Example/compose.yaml up --build -d --wait core dock
```

Откройте `http://localhost:7403`, войдите как `demo` / `demo-lognara-local`. На странице «Подключения» создайте relay (например, `local-relay`), затем агента (`python-example`) и выберите этот relay. Скопируйте показанные один раз строки в `Example/.env`:

```dotenv
LOGNARA_RELAY_CORE_TOKEN=lnr_...
LOGNARA_AGENT_TOKEN=lna_...
LOGNARA_RELAY_MEMORY_MB=2048
```

Потом поднимите всю цепочку и проверьте доставку:

```sh
docker compose -f Example/compose.yaml up --build -d --wait
docker compose -f Example/compose.yaml ps
python3 Example/smoke-test.py
```

Минимальный RAM relay — 2048 MiB. Один параметр задаёт резерв процесса и `mem_limit` контейнера. Если токен потерян, удалите профиль и создайте заново; сначала удаляются агенты, затем relay. При пересоздании профиля сохраняйте старые volumes очередей и обновите `.env`.

Альтернатива регистрации через admin API на `127.0.0.1:7402` (`jq` нужен для извлечения полей):

```sh
RELAY=$(curl --fail-with-body -s http://127.0.0.1:7402/v1/admin/relays \
  -H 'Authorization: Bearer local-admin-token' -H 'Content-Type: application/json' \
  -d '{"name":"local-relay"}')
RELAY_ID=$(printf '%s' "$RELAY" | jq -r '.relay.id')
AGENT=$(curl --fail-with-body -s http://127.0.0.1:7402/v1/admin/agents \
  -H 'Authorization: Bearer local-admin-token' -H 'Content-Type: application/json' \
  -d "{\"name\":\"python-example\",\"relay_id\":$RELAY_ID}")
printf 'LOGNARA_RELAY_CORE_TOKEN=%s\nLOGNARA_AGENT_TOKEN=%s\nLOGNARA_RELAY_MEMORY_MB=2048\n' \
  "$(printf '%s' "$RELAY" | jq -r '.token')" "$(printf '%s' "$AGENT" | jq -r '.token')" > Example/.env
```

Или из `Example`: сначала `docker compose up --build -d --wait core dock`, после заполнения `.env` — `docker compose up --build -d --wait`. Dockerfile использует контекст корня репозитория, Rust 1.94 и Cargo.lock каждого сервиса; фронтенд dock собирается на Node.js 24 через `npm ci`. Первая сборка core с DataFusion/Arrow/Parquet длительная; нужны интернет для загрузки образов и crates, несколько GiB свободной RAM и места на диске. Число параллельных задач Cargo ограничено двумя; кеш сборки используется повторно.

## Просмотр логов в браузере

Открой **http://localhost:7403** и войди:

- Логин: `demo`.
- Пароль: `demo-lognara-local`.

Используй именно `localhost`: этот origin настроен для входа и passkey. Для локального стенда TLS-прокси не требуется. После входа открой страницу логов и отправь запрос на Python-сервер — событие появится в ленте через несколько секунд. Dock обращается к core по внутреннему адресу `http://core:7402` с query-токеном, а браузер получает события через SSE.

В настройках можно сменить пароль и добавить passkey. Пароль создаётся только при отсутствии файла htpasswd: повторный запуск стенда сохраняет изменения. Учётные данные предназначены для локального теста.

## Запросы и логи

Сервер принимает корректные HTTP/1.x-запросы с любым методом и путём, включая нестандартные методы, бинарное тело и chunked-передачу. Ответ — `200 {"status":"ok"}`; HEAD возвращает только заголовки.

```sh
curl http://127.0.0.1:8881/hello?user=demo
curl -X POST http://127.0.0.1:8881/anything -H "Content-Type: application/json" -d '{"user":"demo"}'
curl -X CUSTOM http://127.0.0.1:8881/test -d "custom request"
docker compose -f Example/compose.yaml logs -f app relay core dock
```

В PowerShell используй `curl.exe` вместо `curl`, если `curl` является alias.

Каждый запрос печатается одной JSON-строкой в stdout приложения и отправляется в Lognara. Поля: UTC-время, IP и исходный порт TCP-клиента, метод, путь с query, все заголовки, текст тела и точная копия байтов в `body_base64`. IP — адрес непосредственного соединения, который Docker может заменить адресом шлюза; исходные proxy-заголовки доступны в `headers`. `message` включает адрес клиента, метод и путь для поиска.

Успешный ответ выдаётся после `202` агента. При отказе агента, в том числе из-за лимита записи или заполнения очереди, приложение возвращает `503`, запрос можно повторить; stdout-лог остаётся. Действуют ограничения Lognara на размер и модель записи, поэтому неограниченные тела не поддерживаются. Отправка agent и relay настроена раз в секунду; видимость в поиске появляется после обновления индекса core.

## Проверка доставки и поиск

После запуска стенда выполни из корня (Python 3.10+ без дополнительных пакетов):

```sh
python Example/smoke-test.py
```

На Windows: `py Example/smoke-test.py`. Скрипт отправляет нестандартный HTTP-метод с бинарным телом, до 30 секунд ждёт событие в поиске core и проверяет данные клиента и сохранённые байты. Затем проверяет выдачу SPA dock, вход пользователя `demo`, сессию и появление того же события в SSE-ленте (ещё до 30 секунд). Если пароль изменён в настройках, перед запуском задай `LOGNARA_EXAMPLE_PASSWORD` с текущим паролем.

Поиск вручную:

```sh
curl -X POST http://127.0.0.1:7402/v1/logs/search -H "Authorization: Bearer local-query-token" -H "Content-Type: application/json" -d '{"from":"2020-01-01T00:00:00Z","to":"2100-01-01T00:00:00Z","filters":{"service":["python-example"]}}'
```

Тесты HTTP-сервера с тестовым HTTP-приёмником вместо агента:

```sh
python -B -m unittest discover -s Example/tests -v
```

## Остановка и данные

```sh
docker compose -f Example/compose.yaml down
```

Очереди агента и relay, хранилище core, а также пароль (`dock-auth`), сессии и passkey (`dock-data`) сохраняются в отдельных named volumes. Dock работает от `1000:1000`; папка с htpasswd доступна ему на запись для атомарной смены пароля. При остановке `app` сначала завершается Python-сервер, затем агент получает SIGTERM и время на финальную отправку. Если один из процессов `app` завершился самостоятельно, контейнер также завершается.

Удаление тестовых данных вместе с контейнерами, включая пароль и passkey (следующий запуск восстановит исходный пароль):

```sh
docker compose -f Example/compose.yaml down -v
```
