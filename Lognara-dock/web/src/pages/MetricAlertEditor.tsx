import { type FormEvent, type ReactNode, useEffect, useState } from "react";
import {
  api,
  ApiError,
  errorText,
  nodeScopeLabel,
  type AlertMessagePreviewReply,
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
const defaultMessageFields = [
  "name", "description", "service", "server", "origin", "node", "environment", "condition", "value", "time", "last_point", "status",
];
const messageFieldOptions = [
  ["name", "Техническое имя"], ["title", "Название"], ["description", "Описание"],
  ["service", "Сервис"], ["server", "Сервер"], ["origin", "Источник"], ["node", "Узел"],
  ["environment", "Окружение"], ["condition", "Условие"], ["value", "Значение"],
  ["time", "Время"], ["last_point", "Последняя точка"], ["status", "Статус"],
] as const;
const aggregateLabels: Record<AlertAggregate, string> = {
  avg: "среднее", min: "минимум", max: "максимум", sum: "сумма", count: "число точек", increase: "прирост",
};
const unitSeconds = { sec: 1, min: 60, hour: 3600, day: 86400 } as const;
type WindowUnit = keyof typeof unitSeconds;

function windowParts(seconds: number): { amount: number; unit: WindowUnit } {
  if (seconds % 86400 === 0) return { amount: seconds / 86400, unit: "day" };
  if (seconds % 3600 === 0) return { amount: seconds / 3600, unit: "hour" };
  if (seconds % 60 === 0) return { amount: seconds / 60, unit: "min" };
  return { amount: seconds, unit: "sec" };
}

const fieldList = (fields: string[] | undefined) => fields ?? defaultMessageFields;
const emptyTrigger = (): MetricAlertTrigger => ({
  window_seconds: 300,
  aggregate: "avg",
  comparator: ">",
  threshold: 0,
});

function initialDraft(key: MetricAlertKey, metric: SeriesMetric | null): MetricAlertDefinitionInput {
  return {
    key,
    enabled: false,
    environment: null,
    no_data: key.origin === "rule" ? "normal" : "loss",
    combine: "and",
    group_ids: [],
    triggers: [emptyTrigger()],
    title: metric?.definition?.title ?? null,
    description: metric?.definition?.description ?? null,
    message_template: null,
    message_fields: [...defaultMessageFields],
  };
}

export function definitionDraft(alert: MetricAlertDefinition): MetricAlertDefinitionInput {
  return {
    key: alert.key,
    enabled: alert.enabled,
    environment: alert.environment,
    no_data: alert.no_data,
    combine: alert.combine,
    group_ids: alert.group_ids,
    triggers: alert.triggers,
    title: alert.title ?? null,
    description: alert.description ?? null,
    message_template: alert.message_template ?? null,
    message_fields: fieldList(alert.message_fields),
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
  onDone,
  onCancel,
  picker,
}: {
  picker?: ReactNode;
  keyValue: MetricAlertKey;
  metric: SeriesMetric | null;
  onUnauthorized: () => void;
  onDone?: () => void;
  onCancel?: () => void;
}) {
  const identity = JSON.stringify(keyValue);
  const [alert, setAlert] = useState<MetricAlertDefinition | null>(null);
  const [settings, setSettings] = useState<MetricAlertSettings | null>(null);
  const [draft, setDraft] = useState(() => initialDraft(keyValue, metric));
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [preview, setPreview] = useState<AlertMessagePreviewReply | null>(null);
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
        setDraft(definition.alert ? definitionDraft(definition.alert) : initialDraft(keyValue, metric));
        setSettings(global);
      })
      .catch((cause) => {
        if (!controller.signal.aborted) fail(cause);
      })
      .finally(() => {
        if (!controller.signal.aborted) setReady(true);
      });
    return () => controller.abort();
  }, [identity, metric]);

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
      onDone?.();
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
      setDraft(initialDraft(keyValue, metric));
      setMessage("Алерт удалён.");
      onDone?.();
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

  const previewMessage = async () => {
    setError("");
    try {
      const conditions = draft.triggers.map((trigger) => {
        const window = windowParts(trigger.window_seconds);
        const units: Record<WindowUnit, string> = { sec: "сек", min: "мин", hour: "ч", day: "дн" };
        return `${aggregateLabels[trigger.aggregate]} за ${window.amount} ${units[window.unit]} ${trigger.comparator} ${trigger.threshold}`;
      });
      const reply = await api<AlertMessagePreviewReply>("POST", "/api/alerts/preview", {
        kind: "metric",
        name: keyValue.name,
        title: draft.title ?? metric?.definition?.title ?? keyValue.name,
        description: draft.description ?? metric?.definition?.description ?? "",
        server: keyValue.server,
        service: keyValue.service,
        origin: keyValue.origin,
        environment: draft.environment,
        condition: `${draft.combine === "and" ? "Все условия" : "Любое условие"}: ${conditions.join("; ")}`,
        value: "5",
        time: new Date().toISOString(),
        last_point: metric?.last_seen ?? "нет данных",
        status: "firing",
        message_template: draft.message_template,
        message_fields: fieldList(draft.message_fields),
      });
      setPreview(reply);
    } catch (cause) {
      fail(cause);
    }
  };

  const toggleMessageField = (field: string, checked: boolean) => {
    setDraft((current) => {
      const fields = fieldList(current.message_fields);
      return {
        ...current,
        message_fields: checked
          ? [...fields, field]
          : fields.filter((item) => item !== field),
      };
    });
  };

  const groups: MetricAlertGroup[] = settings?.groups ?? [];
  // Скоупленные правила этого ключа созданы через MCP: здесь их только показываем.
  const scopedRules = (settings?.alerts ?? []).filter(
    (rule) =>
      rule.node_scope.kind !== "all" &&
      rule.key.server === keyValue.server &&
      rule.key.service === keyValue.service &&
      rule.key.name === keyValue.name &&
      rule.key.origin === keyValue.origin,
  );
  const globallyEnabled = settings?.enabled ?? false;
  const canEnable = globallyEnabled || draft.enabled;
  const environments = metric?.environments ?? [];

  return (
    <section className="metric-alert-editor" aria-labelledby="metric-alert-editor-title">
      <div className="metric-alert-editor-heading">
        <div>
          <h2 id="metric-alert-editor-title">Редактор алерта метрики</h2>
          <p className="muted">{keyValue.server} / {keyValue.service} / {keyValue.name} ({keyValue.origin})</p>
        </div>
        {alert && <span className={`tag alert-state-${alert.state}`}>{alert.state}</span>}
      </div>
      {picker}
      {!ready ? (
        <p className="muted">Загрузка настроек алерта…</p>
      ) : (
        <form onSubmit={save}>
          {settings && !settings.enabled && (
            <p className="muted" role="status">
              Глобальная отправка выключена. Настройки можно редактировать; включите Telegram во вкладке «Алерты».
            </p>
          )}
          {alert?.no_recipients && (
            <p className="muted">У этого алерта нет групп с получателями. Уведомления не будут отправляться.</p>
          )}
          {scopedRules.length > 0 && (
            <div>
              <p className="muted">Для этого ключа есть правила, созданные через MCP. Здесь их можно только просмотреть.</p>
              <ul className="alert-settings-list">
                {scopedRules.map((rule) => (
                  <li key={rule.id}>
                    <div>
                      <strong>Создано через MCP · {nodeScopeLabel(rule.node_scope)}</strong>
                      <span className="muted">{rule.enabled ? "Включено" : "Отключено"} · {rule.environment ?? "все окружения"}</span>
                    </div>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <label className="alert-enabled-control switch-label">
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
            {!groups.length && <span className="muted">Создайте группы во вкладке «Получатели и группы».</span>}
          </fieldset>
          <fieldset className="alert-triggers">
            <legend>Условия</legend>
            <label className="alert-combine-control">
              Как объединить строки
              <select
                value={draft.combine}
                onChange={(event) => setDraft((current) => ({ ...current, combine: event.target.value as "and" | "or" }))}
              >
                <option value="and">Все условия одновременно</option>
                <option value="or">Достаточно любого условия</option>
              </select>
            </label>
            {draft.triggers.map((trigger, index) => {
              const window = windowParts(trigger.window_seconds);
              return <div className="alert-trigger-sentence" key={index}>
                <span>Если</span>
                <select
                  aria-label={`Агрегат условия ${index + 1}`}
                  value={trigger.aggregate}
                  onChange={(event) => updateTrigger(index, "aggregate", event.target.value)}
                >
                  {aggregates.map((aggregate) => <option value={aggregate} key={aggregate}>{aggregateLabels[aggregate]}</option>)}
                </select>
                <span>за</span>
                <input
                  aria-label={`Длина окна условия ${index + 1}`}
                  type="number"
                  min={60 / unitSeconds[window.unit]}
                  max={604800 / unitSeconds[window.unit]}
                  step="any"
                  value={window.amount}
                  onChange={(event) => updateTrigger(index, "window_seconds", String(Number(event.target.value) * unitSeconds[window.unit]))}
                  required
                />
                <select
                  aria-label={`Единица окна условия ${index + 1}`}
                  value={window.unit}
                  onChange={(event) => {
                    const unit = event.target.value as WindowUnit;
                    const seconds = Math.max(60, Math.min(604800, window.amount * unitSeconds[unit]));
                    updateTrigger(index, "window_seconds", String(seconds));
                  }}
                >
                  <option value="sec">секунд</option>
                  <option value="min">минут</option>
                  <option value="hour">часов</option>
                  <option value="day">дней</option>
                </select>
                <select
                  aria-label={`Сравнение условия ${index + 1}`}
                  value={trigger.comparator}
                  onChange={(event) => updateTrigger(index, "comparator", event.target.value)}
                >
                  <option value=">">строго больше</option>
                  <option value="<">строго меньше</option>
                </select>
                <input
                  aria-label={`Порог условия ${index + 1}`}
                  type="number"
                  step="any"
                  value={trigger.threshold}
                  onChange={(event) => updateTrigger(index, "threshold", event.target.value)}
                  required
                />
                <span>порога</span>
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
              </div>;
            })}
            <button
              type="button"
              disabled={draft.triggers.length >= 20}
              onClick={() => setDraft((current) => ({ ...current, triggers: [...current.triggers, emptyTrigger()] }))}
            >
              Добавить условие ({draft.triggers.length}/20)
            </button>
          </fieldset>
          <label>
            Свой текст сообщения Telegram
            <textarea
              rows={3}
              maxLength={2000}
              value={draft.message_template ?? ""}
              onChange={(event) => {
                setPreview(null);
                setDraft((current) => ({ ...current, message_template: event.target.value }));
              }}
              placeholder="Например: %title%: текущее значение %value%"
            />
            <span className="muted">Плейсхолдеры: %value%, %name%, %title%, %description%, %service%, %server%, %environment%, %condition%, %time%, %last_point%, %status%.</span>
          </label>
          <fieldset className="alert-message-fields">
            <legend>Поля сообщения</legend>
            {messageFieldOptions.map(([field, label]) => (
              <label key={field}>
                <input
                  type="checkbox"
                  checked={fieldList(draft.message_fields).includes(field)}
                  onChange={(event) => toggleMessageField(field, event.target.checked)}
                />
                {label}
              </label>
            ))}
          </fieldset>
          <div className="toolbar">
            <button type="button" disabled={busy} onClick={previewMessage}>Предпросмотр Telegram</button>
          </div>
          {preview && <div className="telegram-preview" aria-label="Предпросмотр Telegram">
            <div dangerouslySetInnerHTML={{ __html: preview.html }} />
            {preview.button && <span className="telegram-preview-button">Открыть в Dock</span>}
          </div>}
          <div className="toolbar">
            <button type="submit" disabled={busy || draft.triggers.length === 0}>
              Сохранить алерт
            </button>
            {alert && (
              <button type="button" onClick={remove} disabled={busy}>
                Удалить алерт
              </button>
            )}
            {onCancel && <button type="button" onClick={onCancel} disabled={busy}>Отмена</button>}
          </div>
          {message && <p className="ok" role="status">{message}</p>}
          {error && <p className="error" role="alert">{error}</p>}
        </form>
      )}
    </section>
  );
}
