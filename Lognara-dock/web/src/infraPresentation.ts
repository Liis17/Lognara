import type { InfraNode, NodeStatus } from "./types";

const number = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 1 });

export const STATUS: Record<NodeStatus, { label: string; tone: string }> = {
  offline: { label: "Отключён", tone: "err" }, degraded: { label: "Деградирован", tone: "warn" },
  unknown: { label: "Нет данных", tone: "mute" }, idle: { label: "Нет новых логов", tone: "mute" },
  online: { label: "Онлайн", tone: "ok" },
};

export const REASON: Record<string, string> = {
  no_heartbeat: "heartbeat ещё не приходил", relay_silent: "relay агента не на связи",
  spool: "спул заполнен", delivery: "ошибки доставки",
};

export function plural(count: number, forms: [string, string, string]): string {
  const tens = count % 100, ones = count % 10;
  return tens >= 11 && tens <= 14 ? forms[2] : ones === 1 ? forms[0] : ones >= 2 && ones <= 4 ? forms[1] : forms[2];
}

export function bytes(value: number): string {
  const units = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${number.format(value)} ${units[unit]}`;
}

const hourMinute = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit" });

/** До 10 минут — сколько прошло с обновления; дальше — время часов и минут. */
export function refreshedText(loadedAt: Date, now: number): string {
  const elapsed = Math.max(0, now - loadedAt.getTime());
  if (elapsed < 60_000) return `${Math.floor(elapsed / 1000)} сек назад`;
  if (elapsed < 600_000) return `${Math.floor(elapsed / 60_000)} мин назад`;
  return hourMinute.format(loadedAt);
}

/** Возраст отметки по часам core. */
export function age(value: string | null, asOf: string): string {
  if (!value) return "—";
  const seconds = Math.max(0, (Date.parse(asOf) - Date.parse(value)) / 1000);
  if (seconds < 1) return "<1 с";
  if (seconds < 60) return `${Math.floor(seconds)} с`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} мин`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} ч`;
  return `${Math.floor(seconds / 86_400)} д`;
}

export function percent(spool: InfraNode["spool"]): number | null {
  return spool && spool.limit_bytes > 0 ? Math.min(100, Math.round(spool.used_bytes * 100 / spool.limit_bytes)) : null;
}

export function groupChip(nodes: InfraNode[]): { tone: string; text: string } {
  const count = (status: NodeStatus) => nodes.filter((node) => node.status === status).length;
  if (!nodes.length) return { tone: "mute", text: "Нет узлов" };
  if (count("offline")) return { tone: "err", text: `${count("offline")} отключено` };
  if (count("degraded")) return { tone: "warn", text: `${count("degraded")} деградировано` };
  if (count("unknown")) return { tone: "mute", text: `${count("unknown")} без данных` };
  return { tone: "ok", text: "Онлайн" };
}

export function corePresentation(status: "online" | "not_ready" | "unavailable" | undefined) {
  return status === "online" ? { tone: "ok", text: "Онлайн" }
    : status === "not_ready" ? { tone: "warn", text: "Не готов" }
    : { tone: "err", text: "Недоступен" };
}
