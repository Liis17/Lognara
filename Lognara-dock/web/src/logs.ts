import type { LogEvent, QueryFilters, TextQuery } from "./types";

export const MAX_LIVE_ROWS = 2000;
export const LOG_LEVELS = ["trace", "debug", "info", "warn", "error", "fatal", "unknown"] as const;
const EXACT_FIELDS = new Set([
  "id", "environment", "server", "backend", "service", "service_instance", "action",
  "trace_id", "span_id", "parent_span_id", "request_id", "level",
]);

export interface ParsedQuery {
  filters: QueryFilters;
  text?: TextQuery;
}
type Token = { kind: "word" | "quoted" | ":" | "(" | ")"; value: string };

function tokenize(query: string): Token[] {
  const result: Token[] = [];
  for (let position = 0; position < query.length;) {
    const char = query[position];
    if (/\s/u.test(char)) { position++; continue; }
    if (char === ":" || char === "(" || char === ")") {
      result.push({ kind: char, value: char }); position++; continue;
    }
    if (char === '"') {
      const start = position++;
      let closed = false;
      while (position < query.length) {
        const next = query[position++];
        if (next === '"') { closed = true; break; }
        if (next === "\\") position++;
      }
      if (!closed) throw new Error("Закройте двойные кавычки.");
      try { result.push({ kind: "quoted", value: JSON.parse(query.slice(start, position)) as string }); }
      catch { throw new Error("Значение в кавычках должно использовать корректное JSON-экранирование."); }
      continue;
    }
    const start = position;
    while (position < query.length && !/[\s:()"\\]/u.test(query[position])) position++;
    if (position === start) throw new Error("Для обратного слеша используйте значение в кавычках.");
    result.push({ kind: "word", value: query.slice(start, position) });
  }
  return result;
}

const isOperator = (token: Token | undefined, operator: string) =>
  token?.kind === "word" && token.value.toUpperCase() === operator;

/** AND между полями, OR только в field:(value OR value), один режим текстового поиска. */
export function parseQuery(query: string): ParsedQuery {
  const tokens = tokenize(query);
  const filters: QueryFilters = {};
  const words: string[] = [];
  let phrase: string | undefined;
  let position = 0;
  let afterAnd = false;
  while (position < tokens.length) {
    const token = tokens[position++];
    if (isOperator(token, "NOT")) throw new Error("NOT пока не поддерживается.");
    if (isOperator(token, "OR")) throw new Error("OR поддерживается только внутри одного поля: level:(ERROR OR WARN).");
    if (isOperator(token, "AND")) {
      if (position === 1 || afterAnd || position === tokens.length) throw new Error("AND должен соединять условия.");
      afterAnd = true;
      continue;
    }
    if (token.kind !== "word" && token.kind !== "quoted") {
      throw new Error("Скобки разрешены только для значений одного поля.");
    }
    afterAnd = false;
    if (token.kind === "word" && tokens[position]?.kind === ":") {
      position++;
      const field = token.value;
      if (field !== "message" && !EXACT_FIELDS.has(field)) throw new Error(`Поле ${field} не поддерживается.`);
      if (field !== "message" && Object.hasOwn(filters, field)) {
        throw new Error(`Поле ${field} задано повторно. Для вариантов используйте ${field}:(value OR value).`);
      }
      const values: Token[] = [];
      if (tokens[position]?.kind === "(") {
        if (field === "message") throw new Error("Для текста используйте слова или одну фразу в двойных кавычках.");
        position++;
        while (true) {
          const value = tokens[position++];
          if (!value || (value.kind !== "word" && value.kind !== "quoted") ||
              ["OR", "AND", "NOT"].some((operator) => isOperator(value, operator))) {
            throw new Error(`Ожидается значение поля ${field}.`);
          }
          values.push(value);
          if (tokens[position]?.kind === ")") { position++; break; }
          if (!isOperator(tokens[position], "OR")) {
            throw new Error("Внутри скобок разрешён только OR между значениями одного поля.");
          }
          position++;
        }
      } else {
        const value = tokens[position++];
        if (!value || (value.kind !== "word" && value.kind !== "quoted") ||
            ["OR", "AND", "NOT"].some((operator) => isOperator(value, operator))) {
          throw new Error(`Укажите значение поля ${field}.`);
        }
        values.push(value);
      }
      if (field === "message") {
        const value = values[0];
        if (value.kind === "quoted") {
          if (phrase !== undefined) throw new Error("Поддерживается только одна текстовая фраза.");
          phrase = value.value;
        } else words.push(value.value);
      } else {
        if (values.length > 64) throw new Error("В одном поле разрешено не больше 64 значений.");
        filters[field] = [...new Set(values.map(({ value }) => {
          if (new TextEncoder().encode(value).length > 4096) throw new Error("Значение фильтра длиннее 4096 байт.");
          if (field === "level") {
            value = value.toLowerCase();
            if (!LOG_LEVELS.some((level) => level === value)) throw new Error(`Неизвестный уровень ${value}.`);
          }
          if (field === "id") {
            value = value.replace(/^urn:uuid:/iu, "").replace(/^\{(.+)\}$/u, "$1");
            if (!/^[a-fA-F0-9]{32}$/u.test(value) &&
                !/^[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12}$/u.test(value)) {
              throw new Error("Некорректный UUID события.");
            }
            const hex = value.replaceAll("-", "").toLowerCase();
            value = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
          }
          if (["trace_id", "span_id", "parent_span_id"].includes(field)) {
            const length = field === "trace_id" ? 32 : 16;
            if (!new RegExp(`^[a-fA-F0-9]{${length}}$`).test(value)) throw new Error(`Некорректный ${field}.`);
            value = value.toLowerCase();
          }
          return value;
        }))];
      }
    } else if (token.kind === "quoted") {
      if (phrase !== undefined) throw new Error("Поддерживается только одна текстовая фраза.");
      phrase = token.value;
    } else words.push(token.value);
  }
  if (phrase !== undefined && words.length) {
    throw new Error("Нельзя смешивать фразу и отдельные слова: выберите один текстовый режим.");
  }
  const text = phrase !== undefined ? { query: phrase, mode: "phrase" as const } :
    words.length ? { query: words.join(" "), mode: "all" as const } : undefined;
  if (text) {
    const count = messageTokens(text.query).length;
    if (!count || count > 64 || new TextEncoder().encode(text.query).length > 8192) {
      throw new Error("Текстовый поиск требует 1–64 слова и не больше 8192 байт.");
    }
  }
  return { filters, ...(text ? { text } : {}) };
}

function messageTokens(text: string): string[] {
  return text.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
}

/** Локальный фильтр общего SSE; полнота фильтрованной ленты не гарантируется core. */
export function matchesQuery(event: LogEvent, query: ParsedQuery): boolean {
  for (const [field, values] of Object.entries(query.filters)) {
    const value = event[field as keyof LogEvent];
    if (typeof value !== "string" || !values.includes(value)) return false;
  }
  if (!query.text) return true;
  const wanted = messageTokens(query.text.query);
  const actual = messageTokens(event.message);
  return query.text.mode === "all" ? wanted.every((word) => actual.includes(word)) :
    actual.some((_, index) => wanted.every((word, offset) => actual[index + offset] === word));
}

/** Серверный порядок истории не меняется, включая события внутри одной миллисекунды. */
export function appendHistory(current: LogEvent[], page: LogEvent[]): LogEvent[] {
  return [...current, ...page];
}

/** SSE отдаёт batch по возрастанию; новые поступления показываются сверху без Date-сортировки. */
export function prependLive(current: LogEvent[], batch: LogEvent[], limit = MAX_LIVE_ROWS) {
  const seen = new Set<string>();
  const combined = [...batch].reverse().concat(current).filter((event) => {
    if (seen.has(event.id)) return false;
    seen.add(event.id);
    return true;
  });
  return { rows: combined.slice(0, limit), trimmed: combined.length > limit };
}

export function sourceQuery(event: LogEvent): string {
  return ["environment", "server", "backend", "service", "service_instance"]
    .flatMap((field) => {
      const value = event[field as keyof LogEvent];
      return typeof value === "string" ? [`${field}:${JSON.stringify(value)}`] : [];
    }).join(" AND ");
}

export function contextBounds(timestamp: string) {
  const center = Date.parse(timestamp);
  return { from: new Date(center - 300_000).toISOString(), to: new Date(center + 300_000).toISOString() };
}

const CSV_FIELDS = [
  "id", "timestamp", "ingested_at", "level", "environment", "server", "backend", "service",
  "service_instance", "action", "message", "trace_id", "span_id", "parent_span_id", "request_id", "attributes",
] as const;

export function eventsToCsv(events: LogEvent[]): string {
  const cell = (value: unknown): string => {
    let text = value == null ? "" : typeof value === "object" ? JSON.stringify(value) : String(value);
    // Кавычки CSV сами по себе не запрещают выполнение формул табличным редактором.
    if (/^\s*[=+\-@]/u.test(text) || /^[\t\r]/u.test(text)) text = `'${text}`;
    return `"${text.replaceAll('"', '""')}"`;
  };
  return "\uFEFF" + [CSV_FIELDS.map(cell).join(","), ...events.map((event) =>
    CSV_FIELDS.map((field) => cell(event[field])).join(","))].join("\r\n") + "\r\n";
}
