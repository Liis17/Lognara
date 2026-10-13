import { Fragment, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../api";

interface LogEvent {
  id: string;
  timestamp: string;
  level: string;
  service?: string;
  message: string;
  attributes?: Record<string, unknown>;
  [field: string]: unknown;
}

/** Столько строк держится в памяти страницы. */
const MAX_ROWS = 2000;

type Status = "connecting" | "live" | "core_error" | "disconnected";

const statusText: Record<Status, string> = {
  connecting: "Подключение…",
  live: "В реальном времени",
  core_error: "Core недоступен",
  disconnected: "Соединение закрыто",
};

/** Объединяет без повторов (снимок приходит снова после переподключения), новые сверху. */
function merge(current: LogEvent[], batch: LogEvent[]): LogEvent[] {
  const byId = new Map(current.map((event) => [event.id, event]));
  for (const event of batch) byId.set(event.id, event);
  return [...byId.values()]
    .sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp))
    .slice(0, MAX_ROWS);
}

export default function Logs({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [events, setEvents] = useState<LogEvent[]>([]);
  const [status, setStatus] = useState<Status>("connecting");
  const [coreError, setCoreError] = useState("");
  const [paused, setPaused] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const pausedRef = useRef(false);
  const pending = useRef<LogEvent[]>([]);
  const unauthorized = useRef(onUnauthorized);
  unauthorized.current = onUnauthorized;

  useEffect(() => {
    const source = new EventSource("/api/logs/stream");
    source.addEventListener("logs", (message) => {
      const batch = JSON.parse(message.data) as LogEvent[];
      setStatus((current) => (current === "core_error" ? current : "live"));
      if (pausedRef.current) {
        pending.current = merge(pending.current, batch);
      } else {
        setEvents((current) => merge(current, batch));
      }
    });
    source.addEventListener("core_error", (message) => {
      setStatus("core_error");
      setCoreError(JSON.parse(message.data).message);
    });
    source.addEventListener("core_ok", () => setStatus("live"));
    source.onerror = () => {
      if (source.readyState !== EventSource.CLOSED) {
        setStatus("connecting");
        return;
      }
      // EventSource не сообщает код ответа: проверяем, не закончилась ли сессия.
      api("GET", "/api/me")
        .then(() => setStatus("disconnected"))
        .catch((error) => {
          if (error instanceof ApiError && error.status === 401) unauthorized.current();
          else setStatus("disconnected");
        });
    };
    return () => source.close();
  }, []);

  const togglePause = () => {
    if (pausedRef.current) {
      setEvents((current) => merge(current, pending.current));
      pending.current = [];
    }
    pausedRef.current = !pausedRef.current;
    setPaused(pausedRef.current);
  };

  return (
    <section>
      <div className="toolbar">
        <span className={`status ${status}`}>{statusText[status]}</span>
        {status === "core_error" && <span className="error">{coreError}</span>}
        <button onClick={togglePause}>{paused ? "Продолжить" : "Пауза"}</button>
        <span className="muted">{events.length} строк</span>
      </div>
      <table className="logs">
        <thead>
          <tr>
            <th>Время</th>
            <th>Level</th>
            <th>Service</th>
            <th>Message</th>
          </tr>
        </thead>
        <tbody>
          {events.map((event) => (
            <Fragment key={event.id}>
              <tr className={`level-${event.level}`} onClick={() => setOpen(open === event.id ? null : event.id)}>
                <td className="time">{event.timestamp}</td>
                <td>{event.level}</td>
                <td>{event.service ?? ""}</td>
                <td className="message">{event.message}</td>
              </tr>
              {open === event.id && (
                <tr className="details">
                  <td colSpan={4}>
                    <pre>{JSON.stringify(event, null, 2)}</pre>
                  </td>
                </tr>
              )}
            </Fragment>
          ))}
        </tbody>
      </table>
      {events.length === 0 && <p className="muted">Пока нет событий за последние минуты.</p>}
    </section>
  );
}
