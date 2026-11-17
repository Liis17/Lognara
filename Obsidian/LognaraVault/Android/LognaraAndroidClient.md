# Android-клиент Lognara

Parent: [[Index]]

Нативное приложение `lognara-android-client/` для просмотра Lognara с телефона. Работает только через Dock и токен устройства; к core напрямую не обращается, query/admin-токены core на устройство не попадают.

## Стек и сборка

- Kotlin, Jetpack Compose, Material 3; один модуль `app`, пакет `com.lognara.client`, minSdk 35, target/compileSdk 37. Версии в `gradle/libs.versions.toml`, Gradle wrapper 9.4.1 (AGP 9.2.1 требует именно его).
- Без DI и Room: ручной контейнер в `LognaraApplication.kt`.
- Сборка требует JDK 21 (JBR Android Studio): `./gradlew lint testDebugUnitTest assembleDebug`. В CI Android-проверок нет.

## Вход по QR

Контракт со стороны сервера описан в [README Dock](../../../Lognara-dock/README.md#подключение-устройств) и [[Dock/LognaraDock]].

- `data/PairPayload.kt` декодирует payload (base64url JSON `v=1`, `url`, `code`). Разрешён только https; http только для `localhost`/`127.0.0.1` в debug-сборке. Порт и URL дополнительно проверяются парсером OkHttp.
- `data/LognaraApi.kt` обменивает код на токен (`POST /api/devices/pair` без Authorization), затем ходит с `Bearer lgd_...`. Редиректы выключены, тело ответа читается не больше 1 МиБ. Любой 401 означает потерю доступа: локальные данные стираются, приложение возвращается к входу. Dock отвечает 401 и на маршруты вне allowlist токена устройства, поэтому клиент обращается только к разрешённым маршрутам.
- Сканер `ui/screens/ScannerScreen.kt`: CameraX + ZXing (только QR), запасной путь — ручной ввод кода.

## Хранение токена

`data/TokenCipher.kt` и `data/CredentialsStore.kt`: AES-256-GCM ключом Android Keystore (alias `lognara_device_token`), адрес сервера передаётся как AAD. Шифротекст, IV, адрес, пользователь и id устройства лежат в DataStore Preferences. Бэкап и перенос данных отключены в манифесте.

- Необратимые ошибки (подмена данных, утрата или инвалидизация ключа, некорректный адрес) стирают сессию и удаляют ключ.
- Временные сбои Keystore не стирают сессию: показывается экран повтора.
- Повреждённый файл DataStore заменяется пустым.

Отключение устройства вызывает `DELETE /api/devices/current` и всегда стирает локальные данные; при недоступной сети устройство остаётся в списке Dock до ручного отзыва.

## Тема

Material You (dynamic color) по системной теме. Шрифты Commissioner (variable, веса задаются через `FontVariation`) и IBM Plex Mono для машинных данных; источники и лицензии в `app/font-licenses/`. Цвета уровней логов и статусов фиксированы и не зависят от dynamic color (`LognaraColors`).

## Состояние

Реализованы вход, настройки и отключение устройства. Вкладки «Логи», «Live», «Проблемы», «Инфра», «Метрики» пока заглушки.
