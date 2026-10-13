# Локальный Docker-стенд

Parent: [[../Index]]

## Назначение и структура

`Example/` запускает тестовую инфраструктуру на одной машине через Docker Compose. Приложение и агент находятся в одном контейнере, relay, core и dock — в отдельных. Служебный `dock-init` инициализирует хранилища dock и завершается. Внешняя база данных не требуется.

| Файл | Назначение |
|------|------------|
| `Example/compose.yaml` | Сервисы `app`, `relay`, `core`, `dock`, инициализация `dock-init`, внутренние адреса, локальные токены, healthchecks и постоянные volumes. |
| `Example/Dockerfile` | Отдельные стадии сборки agent/relay/core/dock из Cargo.lock на Rust 1.94 и фронтенда dock на Node.js 24; runtime Python 3.13 на Debian bookworm; конечные образы `app`, `relay`, `core`, `dock`. Для WebAuthn dock использует OpenSSL. |
| `Example/Dockerfile.dockerignore` | Исключает локальные target, node_modules, dist, git, `.env`, Obsidian и кеши Python из контекста сборки. |
| `Example/server.py` | HTTP-сервер на `0.0.0.0:8881`, JSON-логи в stdout и HTTP-передача в агент. |
| `Example/entrypoint.py` | Запуск агента и приложения, ожидание порта агента, контроль завершения и передача остановки. |
| `Example/smoke-test.py` | Проверка запроса с нестандартным методом и бинарным телом через всю цепочку до поиска core и SSE-ленты dock, включая SPA, вход и сессию. |
| `Example/tests/test_server.py` | Проверки методов, данных клиента, stdout, chunked-байтов и отказа агента через тестовый HTTP-приёмник. |
| `Example/README.md` | Запуск, запросы, поиск, проверка доставки и очистка данных. |
| `Example/.gitignore` | Исключает `__pycache__` примера. |

## Контракт и поток

1. Клиент отправляет корректный HTTP/1.x-запрос с любым методом и путём на `http://127.0.0.1:8881`.
2. Python записывает UTC-время, непосредственные IP/порт клиента, метод, путь с query, заголовки и тело. Бинарное тело сохраняется без потерь в `attributes.body_base64`, текстовое представление — в `attributes.body`.
3. Один JSON печатается в stdout и отправляется на `POST http://127.0.0.1:7400/v1/logs`. Ответ клиенту — `200` после `202` агента; при отказе/недоступности агента — `503`, stdout-запись сохраняется. HEAD не возвращает тело ответа. Размер записи ограничен действующим контрактом агента.
4. Агент отправляет пачки на `http://relay:7401/v1/batches`, relay — на `http://core:7402/v1/batches`; интервалы отправки обоих сервисов — одна секунда.
5. Core доступен для поиска на `http://127.0.0.1:7402`. Контракт событий и поиска не изменён.
6. Браузер открывает `http://localhost:7403`, входит в dock и получает ленту через `/api/logs/stream`. Dock запрашивает `http://core:7402/v1/logs/search` с query-токеном core; токен в браузер не передаётся.

Источник: `service=python-example`, `server=local-machine`, `backend=example`, `environment=local`. IP из TCP-соединения может быть заменён Docker NAT; proxy-заголовки сохранены в массиве `headers`.

Agent слушает localhost внутри контейнера приложения, relay, core и dock — `0.0.0.0` внутри Compose-сети. На хост публикуются только `127.0.0.1:8881`, `127.0.0.1:7402` и `127.0.0.1:7403`. Локальные тестовые токены явно заданы в Compose; ingest и query различаются. Origin dock — `http://localhost:7403`, для локального теста nginx и TLS не требуются.

## Жизненный цикл и хранилища

`app` зависит от готового relay, relay — от `/health/ready` core. Проверки app/relay открывают TCP-соединения без создания HTTP-логов. Entry point ждёт запуска агента до запуска Python-сервера. Завершение любого дочернего процесса завершает контейнер; при остановке сначала прекращается приём Python, затем агент получает SIGTERM и до 20 секунд на завершение. `init: true` в Compose обслуживает дочерние процессы.

Dock ждёт готовности core и успешного завершения `dock-init`; его healthcheck проверяет `/health/ready`. `dock-init` использует `httpd:2.4-alpine`, создаёт bcrypt cost 12 для `demo` с паролем `demo-lognara-local`, только если htpasswd отсутствует, и назначает права для `1000:1000`. Dock работает от этого пользователя, монтирует папку auth целиком для атомарной смены пароля.

Volumes `agent-spool`, `relay-spool`, `core-data`, `dock-auth`, `dock-data` сохраняются после `docker compose down`. `dock-auth` хранит htpasswd, `dock-data` — SQLite с сессиями и passkey. `down -v` удаляет тестовые данные, пароль и passkey. Сборочные кеши BuildKit отделены от runtime volumes, две задачи Cargo ограничивают параллелизм сборки.

## Методы

| Метод | Описание |
|-------|----------|
| `RequestHandler.__getattr__(name: str): Callable` | Делегирует любой `do_METHOD` общему обработчику запроса. |
| `RequestHandler.read_body(): bytes` | Читает тело по Content-Length или декодирует chunked-передачу. |
| `RequestHandler.handle_request(): None` | Формирует лог, печатает его, передаёт агенту и отвечает клиенту. |
| `RequestHandler.log_message(format: str, *args: object): None` | Отключает дублирующий стандартный access log. |
| `entrypoint.main(): int` | Контролирует запуск и остановку приложения и агента. |
| `smoke-test.main(): None` | Отправляет уникальный запрос и до 30 секунд проверяет его появление и байты в поиске core, затем проверяет dock. |
| `smoke-test.check_dock(event: dict): None` | Проверяет SPA, вход `demo`, сессию и получение того же события через SSE за 30 секунд; текущий пароль можно передать через `LOGNARA_EXAMPLE_PASSWORD`. |

## Проверка

Из корня: `docker compose -f Example/compose.yaml config --quiet`, затем `docker compose -f Example/compose.yaml up --build -d --wait` и `python Example/smoke-test.py`. Тесты сервера: `python -B -m unittest discover -s Example/tests -v`. На Windows вместо `python` можно использовать `py`.
