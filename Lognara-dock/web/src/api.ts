// Запросы к API dock. Сессия живёт в HttpOnly-cookie, браузер отправляет её сам.

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
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
    throw new ApiError(response.status, data?.error ?? "unknown");
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
  query_timeout: "Запрос выполняется слишком долго. Уменьшите диапазон времени",
  challenge_expired: "Время подтверждения истекло, попробуйте снова",
  passkey_rejected: "Ключ не принят",
  passkey_exists: "Этот ключ уже добавлен",
};

export function errorText(error: unknown): string {
  if (error instanceof ApiError) {
    return messages[error.code] ?? `Ошибка ${error.status}`;
  }
  if (error instanceof DOMException && error.name === "NotAllowedError") {
    return "Операция отменена";
  }
  return error instanceof Error ? error.message : String(error);
}
