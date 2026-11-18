# Android-клиент Lognara

Parent: [[Index]]

Нативное приложение `lognara-android-client/` для просмотра Lognara с телефона. Работает только через Dock и токен устройства; к core напрямую не обращается, query/admin-токены core на устройство не попадают.

## Стек и сборка

- Kotlin, Jetpack Compose, Material 3; один модуль `app`, пакет `com.lognara.client`, minSdk 35, target/compileSdk 37. Версии в `gradle/libs.versions.toml`, Gradle wrapper 9.4.1 (AGP 9.2.1 требует именно его).
- Без DI и Room: ручной контейнер в `LognaraApplication.kt`.
- Сборка требует JDK 21 (JBR Android Studio): `./gradlew lint testDebugUnitTest assembleDebug`. Проверки CI описаны в [[Repository/CI#Android]].

## Вход по QR

Контракт со стороны сервера описан в [README Dock](../../../Lognara-dock/README.md#подключение-устройств) и [[Dock/LognaraDock]].

- `data/PairPayload.kt` декодирует payload (base64url JSON `v=1`, `url`, `code`). Разрешён только https; http только для `localhost`/`127.0.0.1` в debug-сборке. Порт и URL дополнительно проверяются парсером OkHttp.
- `data/LognaraApi.kt` обменивает код на токен (`POST /api/devices/pair` без Authorization), затем ходит с `Bearer lgd_...`. Редиректы выключены; лимит тела для pairing, `/api/me` и отключения устройства — 1 МиБ, для запросов данных — 4 МиБ. Любой 401 означает потерю доступа: локальные данные стираются, приложение возвращается к входу. Dock отвечает 401 и на маршруты вне allowlist токена устройства, поэтому клиент обращается только к разрешённым маршрутам.
- Сканер `ui/screens/ScannerScreen.kt`: CameraX + ZXing (только QR), запасной путь — ручной ввод кода.

## Хранение токена

`data/TokenCipher.kt` и `data/CredentialsStore.kt`: AES-256-GCM ключом Android Keystore (alias `lognara_device_token`), адрес сервера передаётся как AAD. Шифротекст, IV, адрес, пользователь и id устройства лежат в DataStore Preferences. Бэкап и перенос данных отключены в манифесте.

- Необратимые ошибки (подмена данных, утрата или инвалидизация ключа, некорректный адрес) стирают сессию и удаляют ключ.
- Временные сбои Keystore не стирают сессию: показывается экран повтора.
- Повреждённый файл DataStore заменяется пустым.

Отключение устройства вызывает `DELETE /api/devices/current` и всегда стирает локальные данные; при недоступной сети устройство остаётся в списке Dock до ручного отзыва.

## Тема

Только тёмная тема по макетам редизайна (`lognara-android-design`, экраны 390×844); dynamic color и светлая тема не используются. Палитра `LognaraPalette` и цвета уровней и статусов `LognaraSemanticColors` лежат в `ui/theme/Theme.kt`; системные панели всегда со светлыми значками (`MainActivity`). Шрифты Commissioner (variable, веса задаются через `FontVariation`) и IBM Plex Mono для машинных данных; источники и лицензии в `app/font-licenses/`.

Общие элементы дизайна собраны в `ui/components/LognaraComponents.kt`: заголовки разделов и вложенных экранов, сегментированный выбор периода, капсулы-фильтры, карточки, бейджи уровней и статусов, строка события, нижняя навигация. Иконки — векторные `res/drawable/ic_*.xml` из макетов. Каждый раздел сам рисует свой заголовок с кнопкой настроек. `HomeScreen` держит нижнюю навигацию и счётчик открытых проблем за 24 ч: один `POST /api/issues/search` при входе и после resolve/reopen. Строки экранов лежат в отдельных `res/values/strings_<раздел>.xml`.

## Состояние

Пять вкладок данных реализованы в `ui/screens/{logs,live,issues,infra,metrics}/`; общие модели и transport находятся в `data/DataModels.kt`, `data/DeviceApiRoutes.kt` и `data/LognaraApi.kt`. `DataScreenComponents.kt` даёт экранам общие состояния загрузки, пустоты и ошибки с повтором запроса, диапазоны времени и локальное форматирование дат. Неизвестные JSON-поля пропускаются; тела запросов сериализуются моделями Dock.

- Авторизованные data-запросы используют только `DeviceApiRoutes`: GET `/api/me`, `/api/infrastructure`, `/api/series`, `/api/series/favorites`, `/api/series/sources`, `/api/traces/{id}`; POST `/api/logs/search`, `/api/stats/summary`, `/api/stats/group-by`, `/api/issues/search`, `/api/issues/{id}/details|resolve|reopen`, `/api/series/lookup`, `/api/series/query`; DELETE `/api/devices/current`; SSE GET `/api/logs/stream`. Динамические идентификаторы пути кодируются как отдельный сегмент URL. `DeviceApiRoutesTest` сравнивает перечень клиента с allowlist из `Lognara-dock/src/session.rs`.
- «Логи» начинает с одного `POST /api/stats/summary`, содержащего `text`, `filters`, `interval_seconds` и `search_limit=50`; продолжение идёт через `POST /api/logs/search` с cursor. Детали события открывают `GET /api/traces/{id}?from&to&limit` для trace.
- «Live» работает, только пока вкладка выбрана и экран в состоянии `RESUMED`. Обрабатываются SSE-события `logs`, `lagged`, `core_error`, `core_ok`; после обрыва выполняется `GET /api/me`, при сохранённой сессии действует экспоненциальный reconnect с пределом 30 с. Буфер ограничен 500 событиями, повторные `id` удаляются; доступны фильтры сервиса/уровней и пауза.
- «Проблемы» сначала запрашивает `POST /api/issues/search`, затем последовательно получает список сервисов через `POST /api/stats/group-by`. Детали загружаются по `/api/issues/{id}/details`; resolve/reopen вызываются отдельными POST после подтверждения. Список обновляется жестом pull-to-refresh, статус общий для Dock.
- «Инфра» использует только `GET /api/infrastructure`: показывает Core, WAL, хранилище, Dock и узлы с причиной/heartbeat, группируя узлы по relay; обновление доступно жестом pull-to-refresh.
- «Метрики» читает избранное через GET `/api/series/favorites` и POST `/api/series/lookup`, источники через GET `/api/series/sources`, каталог источника через GET `/api/series?group_by=server&server&service`, точки через POST `/api/series/query`. Избранное только для чтения; запросы мини-графиков ограничены двумя одновременно. Графики рисуются Compose Canvas, `null` начинает новый сегмент линии, без соединения через пропуск.
- Новых runtime-зависимостей для графиков нет. Тесты моделей и запросов лежат в `app/src/test/`; транспорт использует test-only `mockwebserver3` и `kotlinx-coroutines-test`.
