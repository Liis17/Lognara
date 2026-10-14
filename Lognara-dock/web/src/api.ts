// Запросы к API dock. Сессия живёт в HttpOnly-cookie, браузер отправляет её сам.

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    /** Текст ошибки валидации от core (код invalid). */
    public detail?: string,
  ) {
    super(code);
  }
}

export async function api<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    credentials: "same-origin",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  const data = text ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new ApiError(response.status, data?.error ?? "unknown", data?.detail);
  }
  return data as T;
}

export interface Me {
  username: string;
  passkeys: number;
  series_admin: boolean;
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

const messages: Record<string, string> = {
  unauthorized: "Неверный логин или пароль",
  too_many_attempts: "Слишком много попыток, попробуйте позже",
  invalid_current_password: "Неверный текущий пароль",
  password_too_short: "Новый пароль слишком короткий",
  password_too_long: "Новый пароль длиннее 71 байта",
  invalid_name: "Укажите имя ключа до 64 символов",
  challenge_expired: "Время подтверждения истекло, попробуйте снова",
  passkey_rejected: "Ключ не принят",
  passkey_exists: "Этот ключ уже добавлен",
  core_unavailable: "Core недоступен, попробуйте позже",
  admin_disabled: "Редактирование метрик выключено: не задан admin-токен core",
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
