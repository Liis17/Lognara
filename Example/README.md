# Локальный стенд Lognara

Три контейнера на одной машине:

- `app`: Python 3.13 HTTP-сервер на `8881` и собранный из исходников `lognara-agent` внутри контейнера. Приложение отправляет JSON-логи на `127.0.0.1:7400/v1/logs`.
- `relay`: отдельный `lognara-relay`, принимает пачки агента и отправляет их в core.
- `core`: отдельный `lognara-core`, хранит события и предоставляет поиск на `7402`.

Порты хоста привязаны к `127.0.0.1`; agent и relay наружу не публикуются. Сервисы общаются по внутренней Compose-сети. Ключи `local-relay-token`, `local-ingest-token`, `local-query-token` заданы в Compose для локального теста. Внешние базы данных не нужны.

## Запуск

Нужен Docker с Compose и Linux containers (на Windows можно использовать Docker Desktop). Из корня репозитория:

```sh
docker compose -f Example/compose.yaml config --quiet
docker compose -f Example/compose.yaml up --build -d --wait
docker compose -f Example/compose.yaml ps
```

Или из `Example`: `docker compose up --build -d --wait`. Dockerfile использует контекст корня репозитория, Rust 1.94 и Cargo.lock каждого сервиса. Первая сборка core с DataFusion/Arrow/Parquet длительная; нужны интернет для загрузки образов и crates, несколько GiB свободной RAM и места на диске. Число параллельных задач Cargo ограничено двумя; кеш сборки используется повторно.

## Запросы и логи

Сервер принимает корректные HTTP/1.x-запросы с любым методом и путём, включая нестандартные методы, бинарное тело и chunked-передачу. Ответ — `200 {"status":"ok"}`; HEAD возвращает только заголовки.

```sh
curl http://127.0.0.1:8881/hello?user=demo
curl -X POST http://127.0.0.1:8881/anything -H "Content-Type: application/json" -d '{"user":"demo"}'
curl -X CUSTOM http://127.0.0.1:8881/test -d "custom request"
docker compose -f Example/compose.yaml logs -f app relay core
```

В PowerShell используй `curl.exe` вместо `curl`, если `curl` является alias.

Каждый запрос печатается одной JSON-строкой в stdout приложения и отправляется в Lognara. Поля: UTC-время, IP и исходный порт TCP-клиента, метод, путь с query, все заголовки, текст тела и точная копия байтов в `body_base64`. IP — адрес непосредственного соединения, который Docker может заменить адресом шлюза; исходные proxy-заголовки доступны в `headers`. `message` включает адрес клиента, метод и путь для поиска.

Успешный ответ выдаётся после `202` агента. При отказе агента, в том числе из-за лимита записи или заполнения очереди, приложение возвращает `503`, запрос можно повторить; stdout-лог остаётся. Действуют ограничения Lognara на размер и модель записи, поэтому неограниченные тела не поддерживаются. Отправка agent и relay настроена раз в секунду; видимость в поиске появляется после обновления индекса core.

## Проверка доставки и поиск

После запуска стенда выполни из корня (Python 3.10+ без дополнительных пакетов):

```sh
python Example/smoke-test.py
```

На Windows: `py Example/smoke-test.py`. Скрипт отправляет нестандартный HTTP-метод с бинарным телом, до 30 секунд ждёт событие в поиске core и проверяет данные клиента и сохранённые байты.

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

Очереди агента и relay, а также хранилище core сохраняются в отдельных named volumes. При остановке `app` сначала завершается Python-сервер, затем агент получает SIGTERM и время на финальную отправку. Если один из процессов `app` завершился самостоятельно, контейнер также завершается.

Удаление тестовых данных вместе с контейнерами:

```sh
docker compose -f Example/compose.yaml down -v
```
