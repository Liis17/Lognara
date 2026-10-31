import { type FormEvent, useEffect, useState } from "react";
import {
  api,
  ApiError,
  errorText,
  type AlertAggregate,
  type MetricAlertDefinition,
  type MetricAlertDefinitionInput,
  type MetricAlertGroup,
  type MetricAlertKey,
  type MetricAlertSettings,
  type MetricAlertTrigger,
  type SeriesMetric,
} from "../api";

const aggregates: AlertAggregate[] = ["avg", "min", "max", "sum", "count", "increase"];
const emptyTrigger = (): MetricAlertTrigger => ({
  window_seconds: 300,
  aggregate: "avg",
  comparator: ">",
  threshold: 0,
});

function initialDraft(key: MetricAlertKey): MetricAlertDefinitionInput {
  return {
    key,
    enabled: false,
    environment: null,
    no_data: key.origin === "rule" ? "normal" : "loss",
    combine: "and",
    group_ids: [],
    triggers: [emptyTrigger()],
  };
}

function definitionDraft(alert: MetricAlertDefinition): MetricAlertDefinitionInput {
  return {
    key: alert.key,
    enabled: alert.enabled,
    environment: alert.environment,
    no_data: alert.no_data,
    combine: alert.combine,
    group_ids: alert.group_ids,
    triggers: alert.triggers,
  };
}

function triggerValue(
  trigger: MetricAlertTrigger,
  field: keyof MetricAlertTrigger,
  value: string,
): MetricAlertTrigger {
  if (field === "aggregate") return { ...trigger, aggregate: value as AlertAggregate };
  if (field === "comparator") return { ...trigger, comparator: value as ">" | "<" };
  if (field === "window_seconds") return { ...trigger, window_seconds: Number(value) };
  return { ...trigger, threshold: Number(value) };
}

export default function MetricAlertEditor({
  keyValue,
  metric,
  onUnauthorized,
}: {
  keyValue: MetricAlertKey;
  metric: SeriesMetric | null;
  onUnauthorized: () => void;
}) {
  const identity = JSON.stringify(keyValue);
  const [alert, setAlert] = useState<MetricAlertDefinition | null>(null);
  const [settings, setSettings] = useState<MetricAlertSettings | null>(null);
  const [draft, setDraft] = useState(() => initialDraft(keyValue));
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const fail = (cause: unknown) => {
    if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
    setError(errorText(cause));
  };

  useEffect(() => {
    const query = new URLSearchParams({
      server: keyValue.server,
      service: keyValue.service,
      name: keyValue.name,
      origin: keyValue.origin,
    });
    const controller = new AbortController();
    setReady(false);
    setError("");
    Promise.all([
      api<{ alert: MetricAlertDefinition | null }>(
        "GET",
        `/api/alerts/definition?${query}`,
        undefined,
        { signal: controller.signal },
      ),
      api<MetricAlertSettings>("GET", "/api/alerts", undefined, {
        signal: controller.signal,
      }),
    ])
      .then(([definition, global]) => {
        if (controller.signal.aborted) return;
        setAlert(definition.alert);
        setDraft(definition.alert ? definitionDraft(definition.alert) : initialDraft(keyValue));
        setSettings(global);
      })
      .catch((cause) => {
        if (!controller.signal.aborted) fail(cause);
      })
      .finally(() => {
        if (!controller.signal.aborted) setReady(true);
      });
    return () => controller.abort();
  }, [identity]);

  const save = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    setMessage("");
    setBusy(true);
    try {
      const reply = await api<{ id: string; alert: MetricAlertDefinition }>(
        "PUT",
        "/api/alerts/definition",
        draft,
      );
      setAlert(reply.alert);
      setDraft(definitionDraft(reply.alert));
      setMessage("Алерт сохранён.");
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const remove = async () => {
    setError("");
    setMessage("");
    setBusy(true);
    try {
      await api("DELETE", "/api/alerts/definition", keyValue);
      setAlert(null);
      setDraft(initialDraft(keyValue));
      setMessage("Алерт удалён.");
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const toggleGroup = (groupId: string, checked: boolean) => {
    setDraft((current) => ({
      ...current,
      group_ids: checked
        ? [...current.group_ids, groupId]
        : current.group_ids.filter((id) => id !== groupId),
    }));
  };
  const updateTrigger = (index: number, field: keyof MetricAlertTrigger, value: string) => {
    setDraft((current) => ({
      ...current,
      triggers: current.triggers.map((trigger, position) =>
        position === index ? triggerValue(trigger, field, value) : trigger,
      ),
    }));
  };

  const groups: MetricAlertGroup[] = settings?.groups ?? [];
  const globallyEnabled = settings?.enabled ?? false;
  const canEnable = globallyEnabled || draft.enabled;
  const environments = metric?.environments ?? [];

  return (
    <section className="metric-alert-editor" aria-labelledby="metric-alert-editor-title">
      <div className="metric-alert-editor-heading">
        <div>
          <h2 id="metric-alert-editor-title">Алерт метрики</h2>
          <p className="muted">{keyValue.server} / {keyValue.service} / {keyValue.name} ({keyValue.origin})</p>
        </div>
        {alert && <span className={`tag alert-state-${alert.state}`}>{alert.state}</span>}
      </div>
      {!ready ? (
        <p className="muted">Загрузка настроек алерта…</p>
      ) : (
        <form onSubmit={save}>
          {settings && !settings.enabled && (
            <p className="muted" role="status">
              Глобальная отправка выключена. Настройки можно редактировать; включить новое правило можно после включения алертов в настройках.
            </p>
          )}
          {alert?.no_recipients && (
            <p className="muted">У этого алерта нет групп с получателями. Уведомления не будут отправляться.</p>
          )}
          <label className="alert-enabled-control">
            <input
              type="checkbox"
              checked={draft.enabled}
              disabled={busy || !canEnable}
              onChange={(event) => setDraft((current) => ({ ...current, enabled: event.target.checked }))}
            />
            Включить это правило
          </label>
          <div className="metric-alert-form-grid">
            <label>
              Окружение алерта
              <input
                list="metric-alert-environments"
                value={draft.environment ?? ""}
                placeholder="Все окружения"
                onChange={(event) => setDraft((current) => ({ ...current, environment: event.target.value || null }))}
              />
              <datalist id="metric-alert-environments">
                {environments.map((environment) => <option value={environment} key={environment} />)}
              </datalist>
            </label>
            <label>
              Обработка отсутствия данных
              <select
                value={draft.no_data}
                onChange={(event) => setDraft((current) => ({ ...current, no_data: event.target.value as "normal" | "loss" }))}
              >
                <option value="normal">Считать нормальным значением</option>
                <option value="loss">Отдельно сообщать о потере данных</option>
              </select>
            </label>
            <label>
              Объединение условий
              <select
                value={draft.combine}
                onChange={(event) => setDraft((current) => ({ ...current, combine: event.target.value as "and" | "or" }))}
              >
                <option value="and">Все условия (AND)</option>
                <option value="or">Любое условие (OR)</option>
              </select>
            </label>
          </div>
          <fieldset className="alert-group-picker">
            <legend>Группы получателей</legend>
            {groups.map((group) => (
              <label key={group.id}>
                <input
                  type="checkbox"
                  checked={draft.group_ids.includes(group.id)}
                  onChange={(event) => toggleGroup(group.id, event.target.checked)}
                />
                {group.name} · {group.kind === "personal" ? "личные чаты" : "каналы"}
                {group.recipient_ids.length === 0 ? " · пустая" : ""}
              </label>
            ))}
            {!groups.length && <span className="muted">Создайте группы в настройках.</span>}
          </fieldset>
          <fieldset className="alert-triggers">
            <legend>Условия</legend>
            {draft.triggers.map((trigger, index) => (
              <div className="alert-trigger-row" key={index}>
                <label>
                  Окно, сек
                  <input
                    type="number"
                    min={60}
                    max={604800}
                    value={trigger.window_seconds}
                    onChange={(event) => updateTrigger(index, "window_seconds", event.target.value)}
                    required
                  />
                </label>
                <label>
                  Агрегат
                  <select
                    value={trigger.aggregate}
                    onChange={(event) => updateTrigger(index, "aggregate", event.target.value)}
                  >
                    {aggregates.map((aggregate) => <option value={aggregate} key={aggregate}>{aggregate}</option>)}
                  </select>
                </label>
                <label>
                  Сравнение
                  <select
                    value={trigger.comparator}
                    onChange={(event) => updateTrigger(index, "comparator", event.target.value)}
                  >
                    <option value=">">строго больше (&gt;)</option>
                    <option value="<">строго меньше (&lt;)</option>
                  </select>
                </label>
                <label>
                  Порог
                  <input
                    type="number"
                    step="any"
                    value={trigger.threshold}
                    onChange={(event) => updateTrigger(index, "threshold", event.target.value)}
                    required
                  />
                </label>
                <button
                  type="button"
                  aria-label={`Удалить условие ${index + 1}`}
                  disabled={draft.triggers.length === 1}
                  onClick={() => setDraft((current) => ({
                    ...current,
                    triggers: current.triggers.filter((_, position) => position !== index),
                  }))}
                >
                  Удалить
                </button>
              </div>
            ))}
            <button
              type="button"
              disabled={draft.triggers.length >= 20}
              onClick={() => setDraft((current) => ({ ...current, triggers: [...current.triggers, emptyTrigger()] }))}
            >
              Добавить условие ({draft.triggers.length}/20)
            </button>
          </fieldset>
          <div className="toolbar">
            <button type="submit" disabled={busy || draft.triggers.length === 0}>
              Сохранить алерт
            </button>
            {alert && (
              <button type="button" onClick={remove} disabled={busy}>
                Удалить алерт
              </button>
            )}
          </div>
          {message && <p className="ok" role="status">{message}</p>}
          {error && <p className="error" role="alert">{error}</p>}
        </form>
      )}
    </section>
  );
}
