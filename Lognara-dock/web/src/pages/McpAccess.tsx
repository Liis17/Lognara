import { type FormEvent, useEffect, useState } from "react";
import { api, errorText } from "../api";

type McpSettings = { enabled: boolean; url: string };
type PersonalToken = {
  id: string;
  name: string;
  created_at: number;
  expires_at: number;
  last_used_at: number | null;
};
type OAuthGrant = {
  id: string;
  client_name: string;
  scope: string;
  created_at: number;
  last_used_at: number | null;
};

function formatTime(value: number | null): string {
  return value === null ? "не использовался" : new Date(value).toLocaleString();
}

export default function McpAccess() {
  const [settings, setSettings] = useState<McpSettings | null>(null);
  const [tokens, setTokens] = useState<PersonalToken[]>([]);
  const [grants, setGrants] = useState<OAuthGrant[]>([]);
  const [name, setName] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");

  const refresh = async () => {
    const [nextSettings, nextTokens, nextGrants] = await Promise.all([
      api<McpSettings>("GET", "/api/mcp/access"),
      api<{ tokens: PersonalToken[] }>("GET", "/api/mcp/personal-tokens"),
      api<{ grants: OAuthGrant[] }>("GET", "/api/mcp/grants"),
    ]);
    setSettings(nextSettings);
    setTokens(nextTokens.tokens);
    setGrants(nextGrants.grants);
  };

  useEffect(() => {
    refresh().catch((reason) => setError(errorText(reason)));
  }, []);

  const issue = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError("");
    setMessage("");
    setSecret(null);
    setCopied(false);
    try {
      const reply = await api<{ token: string }>("POST", "/api/mcp/personal-tokens", { name });
      setSecret(reply.token);
      setName("");
      setMessage("Токен создан. Скопируйте его сейчас: после закрытия страницы секрет нельзя будет восстановить.");
      await refresh();
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  };

  const copySecret = async () => {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
      setError("");
    } catch {
      setError("Не удалось скопировать токен. Выделите и скопируйте его вручную.");
    }
  };

  const revokeToken = async (token: PersonalToken) => {
    if (!window.confirm(`Отозвать персональный токен «${token.name}»?`)) return;
    setError("");
    try {
      await api("DELETE", `/api/mcp/personal-tokens/${encodeURIComponent(token.id)}`);
      await refresh();
      setMessage(`Токен «${token.name}» отозван.`);
    } catch (reason) {
      setError(errorText(reason));
    }
  };

  const revokeGrant = async (grant: OAuthGrant) => {
    if (!window.confirm(`Отозвать доступ «${grant.client_name}»?`)) return;
    setError("");
    try {
      await api("DELETE", `/api/mcp/grants/${encodeURIComponent(grant.id)}`);
      await refresh();
      setMessage(`Доступ «${grant.client_name}» отозван.`);
    } catch (reason) {
      setError(errorText(reason));
    }
  };

  return (
    <section className="mcp-access">
      <header>
        <h1>MCP-доступ</h1>
        <p className="page-note">Удалённое подключение к Lognara через read-only MCP.</p>
      </header>

      <article className="mcp-card">
        <h2>Адрес сервера</h2>
        <p><code>{settings?.url ?? "Загружаем…"}</code></p>
        {settings && (
          <p className={settings.enabled ? "status live" : "status disconnected"}>
            {settings.enabled ? "MCP включён" : "MCP выключен"}
          </p>
        )}
        {settings && !settings.enabled && (
          <p className="page-note">Администратору нужно включить LOGNARA_DOCK_MCP_ENABLED=true и опубликовать адрес через HTTPS nginx.</p>
        )}
        <p className="page-note">Подключение открывает чтение логов и метрик, включая полный текст логов. Токены браузерной сессии не подходят для MCP.</p>
      </article>

      <div className="mcp-grid">
        <article className="mcp-card">
          <h2>Персональный токен</h2>
          <p className="page-note">Срок действия 30 дней. Секрет показывается один раз и хранится только в памяти этой страницы.</p>
          <form onSubmit={issue}>
            <label htmlFor="mcp-token-name">Название</label>
            <input id="mcp-token-name" value={name} onChange={(event) => setName(event.target.value)} maxLength={80} placeholder="Codex на рабочем компьютере" />
            <button type="submit" disabled={busy || !settings?.enabled || !name.trim()}>
              Выпустить токен
            </button>
          </form>
          {secret && (
            <div className="mcp-secret" role="region" aria-label="Выданный токен">
              <label htmlFor="mcp-secret">Скопируйте секрет сейчас</label>
              <textarea id="mcp-secret" readOnly value={secret} rows={3} />
              <button type="button" onClick={copySecret}>{copied ? "Скопировано" : "Скопировать токен"}</button>
            </div>
          )}
        </article>

        <article className="mcp-card">
          <h2>Выданные токены</h2>
          <ul className="mcp-list">
            {tokens.map((token) => (
              <li key={token.id}>
                <div>
                  <strong>{token.name}</strong>
                  <span className="muted">Истекает {formatTime(token.expires_at)}</span>
                  <span className="muted">Последнее использование: {formatTime(token.last_used_at)}</span>
                </div>
                <button type="button" className="quiet" onClick={() => revokeToken(token)}>Отозвать</button>
              </li>
            ))}
            {tokens.length === 0 && <li className="muted">Персональных токенов нет</li>}
          </ul>
        </article>
      </div>

      <article className="mcp-card">
        <h2>Доступы OAuth</h2>
        <ul className="mcp-list">
          {grants.map((grant) => (
            <li key={grant.id}>
              <div>
                <strong>{grant.client_name}</strong>
                <span className="muted">Разрешение: {grant.scope}</span>
                <span className="muted">Последнее использование: {formatTime(grant.last_used_at)}</span>
              </div>
              <button type="button" className="quiet" onClick={() => revokeGrant(grant)}>Отозвать</button>
            </li>
          ))}
          {grants.length === 0 && <li className="muted">Подключений OAuth нет</li>}
        </ul>
      </article>

      {message && <p className="ok" role="status">{message}</p>}
      {error && <p className="error" role="alert">{error}</p>}
    </section>
  );
}
