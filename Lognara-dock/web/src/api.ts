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
}
export interface MetricAlertSummary {
  enabled: boolean;
  alerts: Pick<MetricAlertDefinition, "key" | "enabled" | "state" | "no_recipients">[];
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
  created_at: string;
}

export interface Agent {
  id: number;
  name: string;
  relay_id: number;
  created_at: string;
}

const messages: Record<string, string> = {
  import_conflict: "Набор метрик изменился. Проверьте импорт повторно и выберите действия для конфликтов.",
  unauthorized: "Неверный логин или пароль",
  too_many_attempts: "Слишком много попыток, попробуйте позже",
  invalid_current_password: "Неверный текущий пароль",
  password_too_short: "Новый пароль слишком короткий",
  password_too_long: "Новый пароль длиннее 71 байта",
  invalid_name: "Укажите имя от 1 до 64 символов без управляющих",
  invalid_request: "Некорректный запрос",
  name_taken: "Такое имя уже занято",
  relay_has_agents: "У relay есть агенты, сначала удалите их",
  relay_not_found: "Relay не найден, обновите страницу",
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
