// Запросы к API dock. Сессия живёт в HttpOnly-cookie, браузер отправляет её сам.

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    /** Текст ошибки валидации от core (код invalid). */
    public detail?: string,
    public data?: unknown,
  ) {
    super(code);
  }
}

export async function api<T = unknown>(
  method: string,
  path: string,
  body?: unknown,
  options?: { signal?: AbortSignal },
): Promise<T> {
  const response = await fetch(path, {
    method,
    credentials: "same-origin",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: options?.signal,
  });
  const text = await response.text();
  const data = text ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new ApiError(response.status, data?.error ?? "unknown", data?.detail, data);
  }
  return data as T;
}

export interface Me {
  username: string;
  passkeys: number;
}

export interface PasskeyInfo {
  id: number;
  name: string;
  created_at: number;
  last_used_at: number | null;
}

export interface DeviceInfo {
  id: string;
  name: string;
  created_at: number;
  last_used_at: number | null;
}

export interface DevicePairing {
  pairing_id: string;
  payload: string;
  expires_at: number;
}

export type DevicePairingStatus = "pending" | "used" | "expired" | "replaced";

export type Origin = "counter" | "gauge" | "rule";
export type Kind = "delta" | "cumulative" | "gauge";
export type Aggregation = "sum" | "avg" | "min" | "max" | "last";
export type Split = "none" | "instance" | "labels";

export interface Rule {
  environment: string | null;
  contains: string;
  pattern: string;
  value_group: string | null;
  label_groups: string[];
}

export interface Definition {
  server: string;
  id: string;
  service: string;
  name: string;
  title: string;
  description: string;
  unit: string;
  kind: Kind;
  aggregation: Aggregation;
  enabled: boolean;
  rule: Rule | null;
  updated_at: string;
}

/** Тело создания и изменения определения: без id и времени обновления, агрегация необязательна. */
export type DefinitionInput = Omit<Definition, "id" | "updated_at" | "aggregation"> & {
  aggregation?: Aggregation;
};

export interface SeriesMetric {
  server: string;
  service: string;
  name: string;
  origin: Origin;
  first_seen: string | null;
  last_seen: string | null;
  definition: Definition | null;
  /** Окружения точек метрики по всем origin; пусто, если окружений нет. */
  environments: string[];
}

/** Тело запроса серии; без environment сумма по всем окружениям. */
export interface SeriesQuery {
  server?: string;
  origin?: Origin;
  service: string;
  name: string;
  from: string;
  to: string;
  step_seconds: number;
  split: Split;
  environment?: string;
}

export interface SeriesLine {
  label: string;
  values: (number | null)[];
}

export interface SeriesReply {
  kind: Kind;
  aggregation: Aggregation;
  unit: string;
  buckets: string[];
  series: SeriesLine[];
}

export type AlertAggregate = "avg" | "min" | "max" | "sum" | "count" | "increase";
export type AlertState = "unknown" | "ok" | "firing";
/** Узел, которому ограничено право или правило: все узлы, relay с его агентами или один агент. */
export type NodeScope = { kind: "all" } | { kind: "relay"; id: number } | { kind: "agent"; id: number };
export function nodeScopeLabel(scope: NodeScope): string {
  if (scope.kind === "all") return "все узлы";
  return `${scope.kind === "relay" ? "relay" : "агент"} ${scope.id}`;
}
export interface MetricAlertKey {
  server: string;
  service: string;
  name: string;
  origin: Origin;
}
export interface MetricAlertTrigger {
  window_seconds: number;
  aggregate: AlertAggregate;
  comparator: ">" | "<";
  threshold: number;
}
export interface MetricAlertDefinition {
  id: string;
  key: MetricAlertKey;
  node_scope: NodeScope;
  enabled: boolean;
  environment: string | null;
  no_data: "normal" | "loss";
  combine: "and" | "or";
  group_ids: string[];
  triggers: MetricAlertTrigger[];
  revision: number;
  enabled_at: number;
  state: AlertState;
  loss_registered: boolean;
  loss_at: number | null;
  last_point_at: number | null;
  last_as_of: number | null;
  no_recipients: boolean;
  title?: string | null;
  description?: string | null;
  message_template?: string | null;
  message_fields?: string[];
}
export interface MetricAlertSummary {
  enabled: boolean;
  alerts: Pick<MetricAlertDefinition, "key" | "node_scope" | "enabled" | "state" | "no_recipients">[];
}
export interface MetricAlertRecipient {
  id: string;
  kind: "personal" | "channel";
  name: string;
  chat_id: number;
  last_error: string | null;
}
export interface MetricAlertGroup {
  id: string;
  kind: "personal" | "channel";
  name: string;
  recipient_ids: string[];
  alerts: MetricAlertKey[];
  /** Имена алертов на логи с этой группой. */
  log_alerts?: string[];
}
export interface MetricAlertSettings {
  enabled: boolean;
  token_configured: boolean;
  send_paused: boolean;
  send_pause_reason: string | null;
  core_status: "unknown" | "available" | "busy" | "incompatible" | "unavailable" | "disabled";
  core_checked_at: number | null;
  core_error: string | null;
  recipients: MetricAlertRecipient[];
  groups: MetricAlertGroup[];
  alerts: MetricAlertDefinition[];
  pending_deliveries: number;
  permanent_errors: number;
}
export interface MetricAlertDefinitionInput {
  key: MetricAlertKey;
  enabled: boolean;
  environment: string | null;
  no_data: "normal" | "loss";
  combine: "and" | "or";
  group_ids: string[];
  triggers: MetricAlertTrigger[];
  title?: string | null;
  description?: string | null;
  message_template?: string | null;
  message_fields?: string[];
}

export interface PreviewSample {
  timestamp: string;
  message: string;
  value: number;
  labels: string;
}

export interface PreviewReply {
  scanned: number;
  matched: number;
  samples: PreviewSample[];
}

export interface Relay {
  id: number;
  name: string;
  /** `host` - хост-агент со встроенным агентом, отправляет прямо в core. */
  kind?: "relay" | "host";
  created_at: string;
  heartbeat?: { received_at?: string };
}

export interface LogAlertText {
  query: string;
  mode: "all" | "phrase";
}

export interface LogAlertInput {
  name: string;
  enabled: boolean;
  query: string;
  filters: Record<string, string[]>;
  text: LogAlertText | null;
  threshold: number;
  cooldown_seconds: number;
  group_ids: string[];
  message_template?: string | null;
  message_fields?: string[];
}

export interface LogAlert extends LogAlertInput {
  id: string;
  revision: number;
  no_recipients: boolean;
  last_count: number;
  last_match_at: number | null;
  last_checked_at: number | null;
  last_sent_at: number | null;
  message_fields?: string[];
}

export interface AlertMessagePreviewInput {
  kind: "metric" | "logs";
  name: string;
  title?: string | null;
  description?: string | null;
  server?: string | null;
  service?: string | null;
  origin?: Origin | null;
  environment?: string | null;
  condition?: string | null;
  value?: string | null;
  time?: string | null;
  last_point?: string | null;
  status?: "firing" | "recovered" | "data_loss" | "error";
  count?: number;
  query?: string | null;
  message_template?: string | null;
  message_fields: string[];
}

export interface AlertMessagePreviewReply {
  html: string;
  button: boolean;
}

export interface AlertTestReply {
  sent: number;
  failed: { recipient: string; error: string }[];
}

const testFailureText: Record<string, string> = {
  recipient_unavailable: "чат недоступен или бот заблокирован",
  telegram_rate_limited: "Telegram просит подождать",
  telegram_unavailable: "Telegram не ответил",
};

/** Итог тестовой отправки для строки статуса редактора алерта. */
export function alertTestText(reply: AlertTestReply): string {
  const sent = `Тестовое сообщение отправлено в чатов: ${reply.sent}.`;
  if (!reply.failed.length) return sent;
  const failed = reply.failed.map((item) => `${item.recipient} (${testFailureText[item.error] ?? item.error})`).join(", ");
  return `${sent} Не доставлено: ${failed}.`;
}

export interface LogAlertList {
  alerts: LogAlert[];
  core_status: "unknown" | "available" | "busy" | "incompatible" | "unavailable";
}

export interface Agent {
  id: number;
  name: string;
  relay_id: number;
  created_at: string;
  heartbeat?: { received_at?: string };
}

const messages: Record<string, string> = {
  import_conflict: "Набор метрик изменился. Проверьте импорт повторно и выберите действия для конфликтов.",
  unauthorized: "Неверный логин или пароль",
  forbidden: "Недостаточно прав для этого действия",
  too_many_attempts: "Слишком много попыток, попробуйте позже",
  invalid_current_password: "Неверный текущий пароль",
  password_too_short: "Новый пароль слишком короткий",
  password_too_long: "Новый пароль длиннее 71 байта",
  invalid_name: "Укажите имя от 1 до 64 символов без управляющих",
  invalid_request: "Некорректный запрос",
  name_taken: "Такое имя уже занято",
  relay_has_agents: "У relay есть агенты, сначала удалите их",
  host_node: "Узлом хоста управляет раздел «Хост-агенты»",
  log_alert_limit: "Включено максимальное число алертов на логи (100)",
  relay_not_found: "Relay не найден, обновите страницу",
  invalid_scope: "Выбранный relay или агент не найден, обновите список",
  not_found: "Запись не найдена, обновите страницу",
  core_unavailable: "Core недоступен или ответил ошибкой",
  invalid_query: "Проверьте запрос и диапазон времени",
  cursor_expired: "Результаты поиска устарели. Выполните запрос заново",
  query_unavailable: "Поиск временно недоступен. Попробуйте ещё раз",
  pattern_limit: "Можно скрыть не больше 1000 паттернов. Покажите один из скрытых и повторите действие",
  pattern_not_found: "Паттерн не найден в выбранном диапазоне",
  patterns_not_ready: "Индекс паттернов ещё обрабатывает историю. Повторите запрос через несколько секунд",
  issue_limit: "Достигнут предел 10 000 проблем со статусом",
  issue_not_found: "Проблема не найдена: история индекса не содержит этот fingerprint",
  issues_not_ready: "Индекс проблем ещё обрабатывает историю. Повторите запрос через несколько секунд",
  query_timeout: "Запрос выполняется слишком долго. Уменьшите диапазон времени",
  challenge_expired: "Время подтверждения истекло, попробуйте снова",
  passkey_rejected: "Ключ не принят",
  passkey_exists: "Этот ключ уже добавлен",
  invalid_bot_token: "Telegram отклонил токен. Проверьте его и повторите",
  telegram_unavailable: "Не удалось проверить бота в Telegram",
  telegram_rate_limited: "Telegram просит подождать перед повторной проверкой",
  bot_token_required: "Сначала сохраните и проверьте токен Telegram-бота",
  global_disabled_or_alert_limit: "Включите алерты глобально или уменьшите число включённых правил до 100",
  recipient_exists: "Этот Telegram chat ID уже добавлен",
  recipient_kind_in_use: "Сначала удалите получателя из его групп, затем измените тип",
  group_in_use: "Сначала отсоедините группу от алертов",
  group_not_found: "Группа не найдена",
  recipient_not_found: "Получатель не найден",
  no_recipients: "Выберите группу, в которой есть получатели",
};

export function errorText(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.code === "invalid") return `Ошибка в запросе: ${error.detail ?? "неверные данные"}`;
    return messages[error.code] ?? `Ошибка ${error.status}`;
  }
  if (error instanceof DOMException && error.name === "NotAllowedError") {
    return "Операция отменена";
  }
  return error instanceof Error ? error.message : String(error);
}
