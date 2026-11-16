import { type FormEvent, useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router";
import {
  api, ApiError, errorText, type LogAlert, type LogAlertList, type MetricAlertSettings,
} from "../api";
import { type LogAlertForm, logAlertInput } from "../logs";
import type { SummaryResponse } from "../types";

const emptyForm = (query = ""): LogAlertForm => ({
  name: "", enabled: true, query, threshold: 1, cooldownMinutes: 5, groupIds: [],
});

const statusText: Record<LogAlertList["core_status"], string> = {
  unknown: "", available: "",
  busy: "Core занят: проверка повторится через минуту.",
  unavailable: "Core недоступен: алерты на логи не проверяются.",
  incompatible: "Core не поддерживает алерты на логи: обновите Core до версии с /v1/logs/alerts/query.",
};

function formatTime(ms: number | null): string {
  return ms === null ? "нет" : new Date(ms).toLocaleString();
}

export default function LogAlerts({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [params] = useSearchParams();
  const [list, setList] = useState<LogAlertList | null>(null);
  const [settings, setSettings] = useState<MetricAlertSettings | null>(null);
  const [form, setForm] = useState<LogAlertForm>(() => emptyForm(params.get("q") ?? ""));
  const [editing, setEditing] = useState<string | null>(null);
  const [preview, setPreview] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const inputFromForm = () => {
    const input = logAlertInput(form);
    if (!editing) {
      for (const field of ["relay_id", "agent_id"] as const) {
        const values = params.getAll(field);
        if (values.length) input.filters[field] = values;
      }
    }
    return input;
  };

  const load = () => Promise.all([
    api<LogAlertList>("GET", "/api/log-alerts"),
    api<MetricAlertSettings>("GET", "/api/alerts"),
  ]).then(([alerts, alertSettings]) => { setList(alerts); setSettings(alertSettings); });

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onUnauthorized();
      else setError(e instanceof ApiError ? errorText(e) : (e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => { run(async () => undefined); }, []);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    run(async () => {
      const input = inputFromForm();
      if (editing) await api("PUT", `/api/log-alerts/${editing}`, input);
      else await api("POST", "/api/log-alerts", input);
      setForm(emptyForm());
      setEditing(null);
      setPreview("");
    });
  };

  const check = () => {
    setPreview("");
    run(async () => {
      const input = inputFromForm();
      const to = new Date();
      const from = new Date(to.getTime() - 3_600_000);
      const summary = await api<SummaryResponse>("POST", "/api/stats/summary", {
        from: from.toISOString(), to: to.toISOString(), filters: input.filters,
        ...(input.text ? { text: input.text } : {}), interval_seconds: 300, search_limit: 3,
      });
      const lines = (summary.page?.events ?? []).map((event) => `${event.timestamp} ${event.message}`);
      setPreview([`Совпадений за последний час: ${summary.total}`, ...lines].join("\n"));
    });
  };

  const edit = (alert: LogAlert) => {
    setEditing(alert.id);
    setPreview("");
    setForm({
      name: alert.name, enabled: alert.enabled, query: alert.query, threshold: alert.threshold,
      cooldownMinutes: Math.round(alert.cooldown_seconds / 60), groupIds: alert.group_ids,
    });
  };

  const toggle = (alert: LogAlert) => run(async () => {
    const { name, query, filters, text, threshold, cooldown_seconds, group_ids } = alert;
    await api("PUT", `/api/log-alerts/${alert.id}`, {
      name, enabled: !alert.enabled, query, filters, text, threshold, cooldown_seconds, group_ids,
    });
  });

  const remove = (alert: LogAlert) => {
    if (!window.confirm(`Удалить алерт «${alert.name}»?`)) return;
    run(async () => { await api("DELETE", `/api/log-alerts/${alert.id}`); });
  };

  const groups = settings?.groups ?? [];
  const status = list ? statusText[list.core_status] : "";
  return (
    <section className="log-alerts">
      <h2>Алерты на логи</h2>
      <p className="muted">
        Раз в минуту Dock проверяет новые события, подходящие под запрос страницы «Логи», и отправляет
        сообщение в Telegram, если их не меньше порога. Получатели и группы задаются в настройках.
      </p>
      {settings && !settings.enabled && (
        <p className="error">Алерты выключены. Включите их и укажите бота в <Link to="/settings">Настройках</Link>.</p>
      )}
      {status && <p className="error" role="status">{status}</p>}

      <form className="log-alert-form" onSubmit={submit} aria-label={editing ? "Изменить алерт" : "Создать алерт"}>
        <h3>{editing ? "Изменить алерт" : "Новый алерт"}</h3>
        <label>
          Имя
          <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="Вход по SSH" />
        </label>
        <label>
          Запрос
          <input value={form.query} spellCheck={false}
            onChange={(e) => setForm({ ...form, query: e.target.value })}
            placeholder='service:host "Accepted password"' />
        </label>
        <div className="log-alert-numbers">
          <label>
            Совпадений за проверку, не меньше
            <input type="number" min={1} value={form.threshold}
              onChange={(e) => setForm({ ...form, threshold: Number(e.target.value) })} />
          </label>
          <label>
            Пауза между сообщениями, мин
            <input type="number" min={0} max={1440} value={form.cooldownMinutes}
              onChange={(e) => setForm({ ...form, cooldownMinutes: Number(e.target.value) })} />
          </label>
        </div>
        <fieldset>
          <legend>Группы получателей</legend>
          {groups.length === 0 && <p className="muted">Групп нет: создайте их в <Link to="/settings">Настройках</Link>.</p>}
          {groups.map((group) => (
            <label key={group.id} className="checkbox">
              <input type="checkbox" checked={form.groupIds.includes(group.id)}
                onChange={(e) => setForm({
                  ...form,
                  groupIds: e.target.checked ? [...form.groupIds, group.id] : form.groupIds.filter((id) => id !== group.id),
                })} />
              {group.name}
            </label>
          ))}
        </fieldset>
        <label className="checkbox switch-label">
          <input type="checkbox" checked={form.enabled} onChange={(e) => setForm({ ...form, enabled: e.target.checked })} />
          Включён
        </label>
        <div className="access-actions">
          <button type="submit" disabled={busy}>{editing ? "Сохранить" : "Создать алерт"}</button>
          <button type="button" className="quiet" onClick={check} disabled={busy || !form.query.trim()}>Проверить за час</button>
          {editing && (
            <button type="button" className="quiet" onClick={() => { setEditing(null); setForm(emptyForm()); setPreview(""); }}>
              Отмена
            </button>
          )}
        </div>
        {preview && <pre className="log-alert-preview" aria-label="Предпросмотр">{preview}</pre>}
      </form>
      {error && <p className="error">{error}</p>}

      <h3>Правила</h3>
      <ul className="alert-settings-list">
        {(list?.alerts ?? []).map((alert) => (
          <li key={alert.id} aria-label={`Алерт ${alert.name}`}>
            <div>
              <strong>{alert.name}</strong> {!alert.enabled && <span className="muted">(выключен)</span>}
              <div><code>{alert.query}</code></div>
              <span className="muted">
                Порог {alert.threshold} · пауза {Math.round(alert.cooldown_seconds / 60)} мин ·
                последняя проверка {formatTime(alert.last_checked_at)} · совпадений {alert.last_count} ·
                сообщение {formatTime(alert.last_sent_at)}
              </span>
              {alert.no_recipients && <p className="error">Нет получателей: выберите группу.</p>}
            </div>
            <div className="access-actions">
              <Link to={`/logs?${new URLSearchParams({ q: alert.query, range: "1h" })}`}>Открыть в логах</Link>
              <button type="button" onClick={() => edit(alert)} disabled={busy}>Изменить</button>
              <button type="button" onClick={() => toggle(alert)} disabled={busy}>{alert.enabled ? "Выключить" : "Включить"}</button>
              <button type="button" className="quiet" onClick={() => remove(alert)} disabled={busy}>Удалить</button>
            </div>
          </li>
        ))}
      </ul>
      {list && list.alerts.length === 0 && <p className="empty">Алертов на логи пока нет.</p>}
    </section>
  );
}
