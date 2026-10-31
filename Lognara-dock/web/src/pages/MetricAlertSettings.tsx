import { type FormEvent, useEffect, useState } from "react";
import {
  api,
  ApiError,
  errorText,
  type MetricAlertDefinition,
  type MetricAlertGroup,
  type MetricAlertKey,
  type MetricAlertRecipient,
  type MetricAlertSettings as SettingsData,
} from "../api";

function metricUrl(key: MetricAlertKey) {
  const query = new URLSearchParams({
    server: key.server,
    service: key.service,
    name: key.name,
    origin: key.origin,
  });
  return `/metrics/detail?${query}`;
}

function keyText(key: MetricAlertKey) {
  return `${key.server} / ${key.service} / ${key.name} (${key.origin})`;
}

function stateText(alert: MetricAlertDefinition) {
  if (!alert.enabled) return "Отключён";
  if (alert.no_recipients) return "Нет получателей";
  if (alert.loss_registered) return "Потеря данных";
  return { unknown: "Ожидание данных", ok: "В норме", firing: "Сработал" }[
    alert.state
  ];
}

function Recipients({
  recipients,
  onChange,
}: {
  recipients: MetricAlertRecipient[];
  onChange: () => void;
}) {
  const [kind, setKind] = useState<"personal" | "channel">("personal");
  const [name, setName] = useState("");
  const [chatId, setChatId] = useState("");
  const [editing, setEditing] = useState<MetricAlertRecipient | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const edit = (recipient: MetricAlertRecipient) => {
    setEditing(recipient);
    setKind(recipient.kind);
    setName(recipient.name);
    setChatId(String(recipient.chat_id));
    setError("");
  };
  const reset = () => {
    setEditing(null);
    setName("");
    setChatId("");
    setError("");
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    const chat_id = Number(chatId.trim());
    if (!Number.isSafeInteger(chat_id) || chat_id === 0) {
      setError("Введите числовой Telegram chat ID");
      return;
    }
    setBusy(true);
    try {
      await api(
        editing ? "PUT" : "POST",
        editing ? `/api/alerts/recipients/${editing.id}` : "/api/alerts/recipients",
        { kind, name, chat_id },
      );
      reset();
      onChange();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const remove = async (recipient: MetricAlertRecipient) => {
    setBusy(true);
    setError("");
    try {
      await api("DELETE", `/api/alerts/recipients/${recipient.id}`);
      if (editing?.id === recipient.id) reset();
      onChange();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="alert-settings-panel" aria-labelledby="alert-recipients-title">
      <h3 id="alert-recipients-title">Получатели</h3>
      <p className="muted">Личные чаты и каналы ведутся отдельно.</p>
      <ul className="alert-settings-list">
        {recipients.map((recipient) => (
          <li key={recipient.id}>
            <div>
              <strong>{recipient.name}</strong>
              <span className="muted">
                {recipient.kind === "personal" ? "Личный чат" : "Канал"} · {recipient.chat_id}
              </span>
              {recipient.last_error && (
                <span className="error">Telegram не принял получателя</span>
              )}
            </div>
            <button type="button" onClick={() => edit(recipient)} disabled={busy}>
              Изменить
            </button>
            <button type="button" onClick={() => remove(recipient)} disabled={busy}>
              Удалить
            </button>
          </li>
        ))}
        {!recipients.length && <li className="muted">Получателей пока нет</li>}
      </ul>
      <form onSubmit={submit}>
        <h4>{editing ? "Изменить получателя" : "Добавить получателя"}</h4>
        <label>
          Тип
          <select
            aria-label="Тип получателя"
            value={kind}
            onChange={(event) => setKind(event.target.value as typeof kind)}
          >
            <option value="personal">Личный чат</option>
            <option value="channel">Канал</option>
          </select>
        </label>
        <label>
          Название
          <input value={name} onChange={(event) => setName(event.target.value)} maxLength={100} required />
        </label>
        <label>
          Numeric Telegram chat ID
          <input
            inputMode="numeric"
            value={chatId}
            onChange={(event) => setChatId(event.target.value)}
            required
          />
        </label>
        <div className="toolbar">
          <button type="submit" disabled={busy || !name.trim() || !chatId.trim()}>
            {editing ? "Сохранить получателя" : "Добавить получателя"}
          </button>
          {editing && (
            <button type="button" onClick={reset} disabled={busy}>
              Отмена
            </button>
          )}
        </div>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </section>
  );
}

function Groups({
  groups,
  recipients,
  onChange,
}: {
  groups: MetricAlertGroup[];
  recipients: MetricAlertRecipient[];
  onChange: () => void;
}) {
  const [editing, setEditing] = useState<MetricAlertGroup | null>(null);
  const [kind, setKind] = useState<"personal" | "channel">("personal");
  const [name, setName] = useState("");
  const [recipientIds, setRecipientIds] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const available = recipients.filter((recipient) => recipient.kind === kind);
  const reset = () => {
    setEditing(null);
    setName("");
    setRecipientIds([]);
    setError("");
  };
  const edit = (group: MetricAlertGroup) => {
    setEditing(group);
    setKind(group.kind);
    setName(group.name);
    setRecipientIds(group.recipient_ids);
    setError("");
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      await api(
        editing ? "PUT" : "POST",
        editing ? `/api/alerts/groups/${editing.id}` : "/api/alerts/groups",
        { kind, name, recipient_ids: recipientIds },
      );
      reset();
      onChange();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const remove = async (group: MetricAlertGroup) => {
    setBusy(true);
    setError("");
    try {
      await api("DELETE", `/api/alerts/groups/${group.id}`);
      onChange();
    } catch (e) {
      if (e instanceof ApiError && e.code === "group_in_use") {
        const keys = (e.data as { alerts?: MetricAlertKey[] } | undefined)?.alerts ?? [];
        setError(
          keys.length
            ? `Сначала отсоедините группу от алертов: ${keys.map(keyText).join("; ")}`
            : errorText(e),
        );
      } else setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="alert-settings-panel" aria-labelledby="alert-groups-title">
      <h3 id="alert-groups-title">Группы получателей</h3>
      <ul className="alert-settings-list">
        {groups.map((group) => (
          <li key={group.id}>
            <div>
              <strong>{group.name}</strong>
              <span className="muted">
                {group.kind === "personal" ? "Личные чаты" : "Каналы"} · {group.recipient_ids.length} получателей
              </span>
              {group.alerts.length > 0 && (
                <span className="muted">Алерты: {group.alerts.map(keyText).join("; ")}</span>
              )}
            </div>
            <button type="button" onClick={() => edit(group)} disabled={busy}>Изменить</button>
            <button type="button" onClick={() => remove(group)} disabled={busy}>Удалить</button>
          </li>
        ))}
        {!groups.length && <li className="muted">Групп пока нет</li>}
      </ul>
      <form onSubmit={submit}>
        <h4>{editing ? "Изменить группу" : "Добавить группу"}</h4>
        <label>
          Тип группы
          <select
            aria-label="Тип группы"
            value={kind}
            onChange={(event) => {
              setKind(event.target.value as typeof kind);
              setRecipientIds([]);
            }}
          >
            <option value="personal">Личные чаты</option>
            <option value="channel">Каналы</option>
          </select>
        </label>
        <label>
          Название
          <input value={name} onChange={(event) => setName(event.target.value)} maxLength={100} required />
        </label>
        <fieldset className="alert-checkboxes">
          <legend>Получатели</legend>
          {available.map((recipient) => (
            <label key={recipient.id}>
              <input
                type="checkbox"
                checked={recipientIds.includes(recipient.id)}
                onChange={(event) => setRecipientIds((current) =>
                  event.target.checked
                    ? [...current, recipient.id]
                    : current.filter((id) => id !== recipient.id),
                )}
              />
              {recipient.name} · {recipient.chat_id}
            </label>
          ))}
          {!available.length && <span className="muted">Сначала добавьте получателя этого типа.</span>}
        </fieldset>
        <div className="toolbar">
          <button type="submit" disabled={busy || !name.trim()}>
            {editing ? "Сохранить группу" : "Добавить группу"}
          </button>
          {editing && <button type="button" onClick={reset} disabled={busy}>Отмена</button>}
        </div>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </section>
  );
}

export default function MetricAlertSettings({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [token, setToken] = useState("");
  const [tokenMessage, setTokenMessage] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const fail = (cause: unknown) => {
    if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
    setError(errorText(cause));
  };
  const load = () =>
    api<SettingsData>("GET", "/api/alerts")
      .then(setSettings)
      .catch(fail);
  useEffect(() => {
    load();
  }, []);

  const saveToken = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    setTokenMessage("");
    setBusy(true);
    try {
      await api("PUT", "/api/alerts/token", { token });
      setToken("");
      setTokenMessage("Токен проверен и сохранён на сервере.");
      await load();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };
  const setEnabled = async (enabled: boolean) => {
    if (!settings) return;
    setError("");
    setBusy(true);
    try {
      await api("PUT", "/api/alerts/global", { enabled });
      await load();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="alert-settings" id="alerts" aria-labelledby="metric-alerts-title">
      <h2 id="metric-alerts-title">Алерты метрик</h2>
      {settings ? (
        <>
          <div className="alert-global-status">
            <label>
              <input
                type="checkbox"
                checked={settings.enabled}
                disabled={busy || (!settings.enabled && !settings.token_configured)}
                onChange={(event) => setEnabled(event.target.checked)}
              />
              Включить алерты глобально
            </label>
            {!settings.token_configured && (
              <p className="muted">Сначала сохраните проверенный токен Telegram-бота.</p>
            )}
            {settings.send_paused && (
              <p className="error">Отправка приостановлена: {settings.send_pause_reason === "invalid_token" ? "Telegram отклонил токен" : "ошибка отправки"}.</p>
            )}
            <p className="muted">
              Core: {settings.core_status}
              {settings.core_checked_at ? ` · проверен ${new Date(settings.core_checked_at).toLocaleString()}` : ""}
              {settings.core_error ? ` · ${settings.core_error}` : ""}
            </p>
            <p className="muted">
              В очереди: {settings.pending_deliveries} · постоянные ошибки получателей: {settings.permanent_errors}
            </p>
          </div>
          <form onSubmit={saveToken} className="alert-token-form">
            <h3>Telegram Bot API</h3>
            <p className="muted">Токен проверяется через Telegram и хранится только в SQLite Dock. Он включён в резервные копии базы.</p>
            <label>
              {settings.token_configured ? "Новый токен бота" : "Токен бота"}
              <input
                type="password"
                autoComplete="new-password"
                value={token}
                onChange={(event) => setToken(event.target.value)}
                placeholder={settings.token_configured ? "Сохранённый токен скрыт" : "123456:…"}
              />
            </label>
            <button type="submit" disabled={busy || !token.trim()}>
              Проверить и сохранить токен
            </button>
            {tokenMessage && <p className="ok">{tokenMessage}</p>}
          </form>
          <div className="alert-settings-columns">
            <Recipients recipients={settings.recipients} onChange={load} />
            <Groups groups={settings.groups} recipients={settings.recipients} onChange={load} />
          </div>
          <section className="alert-settings-panel" aria-labelledby="alert-list-title">
            <h3 id="alert-list-title">Все правила</h3>
            <ul className="alert-settings-list">
              {settings.alerts.map((alert) => (
                <li key={alert.id}>
                  <div>
                    <strong>{keyText(alert.key)}</strong>
                    <span className="muted">{stateText(alert)} · {alert.environment ?? "все окружения"} · {alert.combine.toUpperCase()}</span>
                    {alert.no_recipients && <span className="muted">Не выбраны группы с получателями</span>}
                  </div>
                  <a href={metricUrl(alert.key)}>Настроить</a>
                </li>
              ))}
              {!settings.alerts.length && <li className="muted">Алертов пока нет</li>}
            </ul>
          </section>
        </>
      ) : (
        <p className="muted">Загрузка настроек алертов…</p>
      )}
      {error && <p className="error" role="alert">{error}</p>}
    </section>
  );
}
