import { useEffect, useState } from "react";
import {
  api,
  ApiError,
  errorText,
  type Definition,
  type PreviewReply,
} from "../api";
import {
  bundleOf,
  exampleBundle,
  nginxTemplates,
  type ImportAction,
  type MetricBundle,
} from "../metricBundles";
import MetricDialog from "../components/MetricDialog";
import MetricEditor from "./MetricEditor";

interface CheckReply {
  revision: string;
  errors: { index: number | null; field: string; message: string }[];
  conflicts: { index: number; existing: Definition }[];
  metrics: { name: string; title?: string; description?: string }[];
}
interface Source {
  server: string;
  service: string;
}

export default function MetricCreate({
  onClose,
  onDone,
  onUnauthorized,
}: {
  onClose: () => void;
  onDone: () => void;
  onUnauthorized: () => void;
}) {
  const [sources, setSources] = useState<Source[]>([]);
  const [server, setServer] = useState("");
  const [service, setService] = useState("");
  const [mode, setMode] = useState("manual");
  const [text, setText] = useState("");
  const [selected, setSelected] = useState(
    nginxTemplates.map((t) => t.metric.name),
  );
  const [check, setCheck] = useState<CheckReply | null>(null);
  const [bundle, setBundle] = useState<MetricBundle | null>(null);
  const [actions, setActions] = useState<ImportAction[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [summary, setSummary] = useState("");
  const [samples, setSamples] = useState<Record<number, PreviewReply>>({});
  const [conflictsOpen, setConflictsOpen] = useState(false);
  const fail = (e: unknown) => {
    if (e instanceof ApiError && e.status === 401) onUnauthorized();
    else setError(errorText(e));
  };
  useEffect(() => {
    const controller = new AbortController();
    api<{ sources: Source[] }>("GET", "/api/series/sources", undefined, {
      signal: controller.signal,
    })
      .then((r) => setSources(r.sources))
      .catch((e) => {
        if (!controller.signal.aborted) fail(e);
      });
    return () => controller.abort();
  }, []);
  const reset = () => {
    setCheck(null);
    setBundle(null);
    setSamples({});
    setError("");
    setSummary("");
    setConflictsOpen(false);
  };
  const verify = async () => {
    setSummary("");
    setBusy(true);
    setError("");
    setCheck(null);
    setSamples({});
    try {
      const next: MetricBundle =
        mode === "templates"
          ? bundleOf(
              nginxTemplates
                .filter((t) => selected.includes(t.metric.name))
                .map((t) => t.metric),
            )
          : JSON.parse(text);
      const reply = await api<CheckReply>("POST", "/api/series/import/check", {
        server,
        service,
        bundle: next,
      });
      setBundle(next);
      setCheck(reply);
      setActions(
        reply.metrics.map((_, index) => ({
          action: reply.conflicts.some((c) => c.index === index)
            ? "skip"
            : "create",
        })),
      );
      if (reply.conflicts.length) setConflictsOpen(true);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };
  const apply = async () => {
    if (!bundle || !check) return;
    setBusy(true);
    setError("");
    try {
      const result = await api<{
        created: number;
        replaced: number;
        skipped: number;
      }>("POST", "/api/series/import/apply", {
        server,
        service,
        bundle,
        revision: check.revision,
        actions,
      });
      setSummary(
        `Создано: ${result.created}, заменено: ${result.replaced}, пропущено: ${result.skipped}`,
      );
      setCheck(null);
      onDone();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) setCheck(null);
      fail(e);
    } finally {
      setBusy(false);
    }
  };
  const preview = async (index: number) => {
    if (!bundle) return;
    setBusy(true);
    setError("");
    try {
      const rule = bundle.metrics[index].rule;
      const result = await api<PreviewReply>("POST", "/api/series/preview", {
        server,
        service,
        environment: rule?.environment,
        rule,
        minutes: 60,
      });
      setSamples((current) => ({ ...current, [index]: result }));
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };
  const copySchema = async () => {
    try {
      const response = await fetch("/metrics-schema.json");
      if (!response.ok) throw new Error("Не удалось загрузить стандарт JSON");
      await navigator.clipboard.writeText(await response.text());
    } catch (e) {
      fail(e);
    }
  };
  const copy = async (value: string) => {
    try {
      await navigator.clipboard.writeText(value);
    } catch (e) {
      fail(e);
    }
  };
  return (
    <MetricDialog title="Новая метрика" onClose={onClose} busy={busy}>
      <div className="metric-create">
        <div className="metric-source-selectors">
          <label>
            Сервер
            <select
              aria-label="Выберите сервер"
              disabled={busy}
              value={server}
              onChange={(e) => {
                setServer(e.target.value);
                setService("");
                reset();
              }}
            >
              <option value="">Выберите сервер</option>
              {[...new Set(sources.map((s) => s.server))].sort().map((s) => (
                <option key={s}>{s}</option>
              ))}
            </select>
          </label>
          <label>
            Сервис
            <select
              aria-label="Выберите сервис"
              disabled={!server || busy}
              value={service}
              onChange={(e) => {
                setService(e.target.value);
                reset();
              }}
            >
              <option value="">Выберите сервис</option>
              {[
                ...new Set(
                  sources
                    .filter((s) => s.server === server)
                    .map((s) => s.service),
                ),
              ]
                .sort()
                .map((s) => (
                  <option key={s}>{s}</option>
                ))}
            </select>
          </label>
        </div>
        {sources.length === 0 && (
          <p className="muted">
            Источники появятся после получения логов или heartbeat агента.
          </p>
        )}
        <div className="toolbar" role="group" aria-label="Способ создания">
          {[
            ["manual", "Вручную"],
            ["json", "Импорт JSON"],
            ["templates", "Шаблоны"],
          ].map(([value, label]) => (
            <button
              key={value}
              disabled={busy}
              aria-pressed={mode === value}
              onClick={() => {
                setMode(value);
                reset();
              }}
            >
              {label}
            </button>
          ))}
        </div>
        {mode === "manual" && server && service && (
          <MetricEditor
            key={`${server}/${service}`}
            definition={null}
            start={{ server, service, name: "", kind: "delta", rule: true }}
            onUnauthorized={onUnauthorized}
            onDone={() => {
              onDone();
              onClose();
            }}
            onCancel={onClose}
          />
        )}
        {mode === "json" && (
          <>
            <label>
              JSON набора
              <textarea
                className="pattern"
                rows={12}
                value={text}
                disabled={busy}
                onChange={(e) => {
                  setText(e.target.value);
                  reset();
                }}
                placeholder={JSON.stringify(exampleBundle, null, 2)}
              />
            </label>
            <div className="toolbar">
              <button
                onClick={() => copy(JSON.stringify(exampleBundle, null, 2))}
              >
                Скопировать пример
              </button>
              <button onClick={copySchema}>Скопировать стандарт JSON</button>
            </div>
            <p className="muted">
              Формат lognara-metrics, версия 1. Сервер и сервис выбираются выше.{" "}
              <a href="/metrics-schema.json" target="_blank" rel="noreferrer">
                JSON Schema
              </a>{" "}
              ·{" "}
              <a href="/metrics-guide.md" target="_blank" rel="noreferrer">
                Инструкция для агента
              </a>
            </p>
          </>
        )}
        {mode === "templates" && (
          <>
            <h3>Nginx: стандартные access/error-логи</h3>
            <p className="muted">
              Логи должны уже поступать в Lognara. Access: combined; время
              ответа не извлекается.
            </p>
            {nginxTemplates.map((t) => (
              <label className="metric-template" key={t.metric.name}>
                <span>
                  <input
                    type="checkbox"
                    disabled={busy}
                    checked={selected.includes(t.metric.name)}
                    onChange={(e) => {
                      setSelected((current) =>
                        e.target.checked
                          ? [...current, t.metric.name]
                          : current.filter((n) => n !== t.metric.name),
                      );
                      reset();
                    }}
                  />{" "}
                  {t.metric.title}
                </span>
                <span className="muted">{t.metric.description}</span>
                <code>{t.example}</code>
                <details>
                  <summary>Регулярное выражение</summary>
                  <code>{t.metric.rule?.pattern}</code>
                </details>
              </label>
            ))}
          </>
        )}
        {mode !== "manual" && (
          <button
            onClick={verify}
            disabled={
              busy ||
              !server ||
              !service ||
              (mode === "templates" ? selected.length === 0 : !text.trim())
            }
          >
            Проверить набор
          </button>
        )}
        {check && (
          <>
            {check.errors.map((e, i) => (
              <p className="error" key={i}>
                {e.index === null ? "Набор" : `Метрика ${e.index + 1}`} ·{" "}
                {e.field}: {e.message}
              </p>
            ))}
            <div className="preview-result">
              <table className="preview">
                <thead>
                  <tr>
                    <th>Метрика</th>
                    <th>Действие</th>
                    <th>Проверка логов</th>
                  </tr>
                </thead>
                <tbody>
                  {check.metrics.map((m, index) => (
                    <tr key={index}>
                      <td>
                        {m.title || m.name}
                        <br />
                        <code>{m.name}</code>
                        {m.description && <p>{m.description}</p>}
                      </td>
                      <td>
                        {actions[index]?.action === "rename"
                          ? `Переименовать: ${actions[index].name}`
                          : (
                              {
                                skip: "Пропустить",
                                replace: "Заменить",
                                create: "Создать",
                              } as Record<string, string>
                            )[actions[index]?.action]}
                      </td>
                      <td>
                        <button
                          disabled={
                            busy || check.errors.some((e) => e.index === index)
                          }
                          onClick={() => preview(index)}
                        >
                          Проверить логи
                        </button>
                        {samples[index] && (
                          <p>
                            Просмотрено {samples[index].scanned}, совпало{" "}
                            {samples[index].matched}.{" "}
                            {samples[index].matched === 0 &&
                              "Подходящих строк пока нет; правило можно создать для будущих логов."}
                          </p>
                        )}
                        {samples[index]?.samples.length > 0 && (
                          <details>
                            <summary>Примеры совпадений</summary>
                            {samples[index].samples
                              .slice(0, 3)
                              .map((sample, i) => (
                                <p key={i}>
                                  <strong>{sample.value}</strong>{" "}
                                  {sample.labels}
                                  <br />
                                  <code>{sample.message.slice(0, 200)}</code>
                                </p>
                              ))}
                          </details>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="toolbar">
              {check.conflicts.length > 0 && (
                <button onClick={() => setConflictsOpen(true)}>
                  Разрешить конфликты ({check.conflicts.length})
                </button>
              )}
              <button
                disabled={busy || check.errors.some((e) => e.index !== null)}
                onClick={apply}
              >
                Применить набор
              </button>
            </div>
          </>
        )}
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {summary && <p role="status">{summary}</p>}
        {conflictsOpen && check && (
          <MetricDialog
            title="Метрики уже существуют"
            onClose={() => setConflictsOpen(false)}
          >
            <p>
              Выберите действие для каждого совпадения. Замена изменит
              настройки, исторические точки сохранятся.
            </p>
            {check.conflicts.map((c) => (
              <div className="metric-conflict" key={c.index}>
                <strong>{c.existing.title || c.existing.name}</strong>
                <label>
                  Действие для {c.existing.name}
                  <select
                    value={actions[c.index].action}
                    onChange={(e) =>
                      setActions((current) =>
                        current.map((a, index) =>
                          index === c.index
                            ? e.target.value === "rename"
                              ? {
                                  action: "rename",
                                  name: `${c.existing.name}_copy`,
                                }
                              : { action: e.target.value as "skip" | "replace" }
                            : a,
                        ),
                      )
                    }
                  >
                    <option value="skip">Пропустить</option>
                    <option value="rename">Переименовать</option>
                    <option value="replace">Заменить</option>
                  </select>
                </label>
                {actions[c.index].action === "rename" && (
                  <label>
                    Новое имя
                    <input
                      value={
                        (actions[c.index] as { action: "rename"; name: string })
                          .name
                      }
                      onChange={(e) =>
                        setActions((current) =>
                          current.map((a, index) =>
                            index === c.index
                              ? { action: "rename", name: e.target.value }
                              : a,
                          ),
                        )
                      }
                    />
                  </label>
                )}
              </div>
            ))}
            <button onClick={() => setConflictsOpen(false)}>Готово</button>
          </MetricDialog>
        )}
      </div>
    </MetricDialog>
  );
}
