// Запросы к API dock. Сессия живёт в HttpOnly-cookie, браузер отправляет её сам.

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
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
