import { type FormEvent, useState } from "react";
import {
  api,
  ApiError,
  errorText,
  type Aggregation,
  type Definition,
  type DefinitionInput,
  type Kind,
  type PreviewReply,
  type Rule,
} from "../api";

export const kindText: Record<Kind, string> = {
  delta: "приращение",
  cumulative: "накопление",
  gauge: "текущее значение",
};

export const aggregationText: Record<Aggregation, string> = {
  sum: "сумма",
  avg: "среднее",
  min: "минимум",
  max: "максимум",
  last: "последнее",
};

/** Исходные значения для нового определения. */
export interface EditorStart {
  server: string;
  service: string;
  /** Пустое имя: пользователь введёт его сам. */
  name: string;
  kind: Kind;
  /** Сразу включить извлечение из логов. */
  rule: boolean;
}

interface Form {
  server: string;
  service: string;
  name: string;
  title: string;
  description: string;
  unit: string;
  kind: Kind;
  aggregation: Aggregation | "";
  enabled: boolean;
  useRule: boolean;
  environment: string;
  contains: string;
  pattern: string;
  valueGroup: string;
  labelGroups: string;
}

/** Длиннее этого текст сообщения в таблице проверки обрезается. */
const MAX_MESSAGE = 200;

function initialForm(definition: Definition | null, start: EditorStart): Form {
  const rule = definition?.rule;
  return {
    server: definition?.server ?? start.server,
    service: definition?.service ?? start.service,
    name: definition?.name ?? start.name,
    title: definition?.title ?? "",
    description: definition?.description ?? "",
    unit: definition?.unit ?? "",
    kind: definition?.kind ?? start.kind,
    aggregation: definition?.aggregation ?? "",
    enabled: definition?.enabled ?? true,
    useRule: definition ? rule != null : start.rule,
    environment: rule?.environment ?? "",
    contains: rule?.contains ?? "",
    pattern: rule?.pattern ?? "",
    valueGroup: rule?.value_group ?? "",
    labelGroups: rule?.label_groups.join(", ") ?? "",
  };
}

function ruleOf(form: Form): Rule {
  return {
    environment: form.environment.trim() || null,
    contains: form.contains,
    pattern: form.pattern,
    value_group: form.valueGroup.trim() || null,
    label_groups: form.labelGroups
      .split(",")
      .map((group) => group.trim())
      .filter(Boolean),
  };
}

function toInput(form: Form): DefinitionInput {
  return {
    server: form.server,
    service: form.service.trim(),
    name: form.name.trim(),
    title: form.title.trim(),
    description: form.description.trim(),
    unit: form.unit.trim(),
    kind: form.kind,
    aggregation: form.aggregation || undefined,
    enabled: form.enabled,
    rule: form.useRule ? ruleOf(form) : null,
  };
}

function shorten(message: string): string {
  return message.length > MAX_MESSAGE ? `${message.slice(0, MAX_MESSAGE)}…` : message;
}

interface Props {
  /** Существующее определение; null - создаём новое. */
  definition: Definition | null;
  start: EditorStart;
  onUnauthorized: () => void;
  /** Сохранено или удалено: редактор закрывается, список перечитывается. */
  onDone: () => void;
  onCancel: () => void;
}

export default function MetricEditor({ definition, start, onUnauthorized, onDone, onCancel }: Props) {
  const [form, setForm] = useState(() => initialForm(definition, start));
  const [preview, setPreview] = useState<PreviewReply | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  // Имя фиксировано у существующего определения и у метрики, которую настраиваем.
  const nameLocked = definition !== null || start.name !== "";
  const heading = definition
    ? "Определение метрики"
    : nameLocked
      ? "Настройка метрики"
      : "Новая метрика из текста логов";

  const set = <K extends keyof Form>(key: K, value: Form[K]) =>
    { setForm((current) => ({ ...current, [key]: value })); setPreview(null); };

  const fail = (e: unknown) => {
    if (e instanceof ApiError && e.status === 401) onUnauthorized();
    else setError(errorText(e));
  };

  const check = async () => {
    setBusy(true);
    setError("");
    try {
      const rule = ruleOf(form);
      setPreview(
        await api<PreviewReply>("POST", "/api/series/preview", {
          server: form.server,
          service: form.service.trim(),
          environment: rule.environment,
          rule,
          minutes: 60,
        }),
      );
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const save = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      const input = toInput(form);
      if (definition) await api("PUT", `/api/series/definitions/${definition.id}`, input);
      else await api("POST", "/api/series/definitions", input);
      onDone();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!definition || !window.confirm(`Удалить определение «${definition.title || definition.name}»?`)) return;
    setBusy(true);
    setError("");
    try {
      await api("DELETE", `/api/series/definitions/${definition.id}`);
      onDone();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="metric-editor">
      <h2>{heading}</h2>
      <form onSubmit={save}>
        <label>Сервер<input value={form.server} readOnly /></label>
        <label>
          Сервис
          <input value={form.service} readOnly />
        </label>
        <label>
          Имя
          <input value={form.name} readOnly={nameLocked} onChange={(e) => set("name", e.target.value)} />
        </label>
        <label>
          Заголовок
          <input value={form.title} onChange={(e) => set("title", e.target.value)} />
        </label>
        <label>
          Описание
          <textarea rows={2} value={form.description} onChange={(e) => set("description", e.target.value)} />
        </label>
        <label>
          Единица
          <input value={form.unit} onChange={(e) => set("unit", e.target.value)} placeholder="запросов" />
        </label>
        <label>
          Тип
          <select value={form.kind} onChange={(e) => set("kind", e.target.value as Kind)}>
            {Object.entries(kindText).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label>
          Агрегация
          <select value={form.aggregation} onChange={(e) => set("aggregation", e.target.value as Aggregation | "")}>
            <option value="">по умолчанию</option>
            {Object.entries(aggregationText).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label className="check">
          <input type="checkbox" checked={form.enabled} onChange={(e) => set("enabled", e.target.checked)} />
          Включена
        </label>
        <label className="check">
          <input type="checkbox" checked={form.useRule} onChange={(e) => set("useRule", e.target.checked)} />
          Извлекать из текста логов (regex)
        </label>
        {form.useRule && (
          <>
            <label>
              Окружение (необязательно)
              <input value={form.environment} onChange={(e) => set("environment", e.target.value)} />
            </label>
            <label>
              Подстрока для отбора строк
              <input value={form.contains} onChange={(e) => set("contains", e.target.value)} />
            </label>
            <label>
              Регулярное выражение
              <textarea
                className="pattern"
                rows={3}
                value={form.pattern}
                onChange={(e) => set("pattern", e.target.value)}
              />
            </label>
            <label>
              Группа значения (необязательно)
              <input value={form.valueGroup} onChange={(e) => set("valueGroup", e.target.value)} />
              <span className="muted">без группы каждое совпадение считается как +1</span>
            </label>
            <label>
              Группы меток через запятую, до 4 (необязательно)
              <input value={form.labelGroups} onChange={(e) => set("labelGroups", e.target.value)} />
            </label>
          </>
        )}
        <div className="toolbar">
          {form.useRule && (
            <button type="button" onClick={check} disabled={busy}>
              Проверить
            </button>
          )}
          <button type="submit" disabled={busy || !form.service.trim() || !form.name.trim()}>
            Сохранить
          </button>
          {definition && (
            <button type="button" onClick={remove} disabled={busy}>
              Удалить
            </button>
          )}
          <button type="button" onClick={onCancel} disabled={busy}>
            Отмена
          </button>
        </div>
      </form>
      {error && <p className="error">{error}</p>}
      {preview && (
        <div className="preview-result">
          <p className="muted">
            просмотрено {preview.scanned}, совпало {preview.matched}
          </p>
          <table className="preview">
            <thead>
              <tr>
                <th>Время</th>
                <th>Значение</th>
                <th>Метки</th>
                <th>Сообщение</th>
              </tr>
            </thead>
            <tbody>
              {preview.samples.map((sample, index) => (
                <tr key={index}>
                  <td className="time">{sample.timestamp}</td>
                  <td>{sample.value}</td>
                  <td>{sample.labels}</td>
                  <td className="message">{shorten(sample.message)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
