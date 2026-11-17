import { type FormEvent, useEffect, useState } from "react";
import { api, errorText, type NodeScope } from "../api";
import {
  AccessFields,
  accessBody,
  defaultAccessDraft,
  loadNodeLists,
  type AccessDraft,
  type NodeLists,
} from "../components/AccessFields";

type McpSettings = { enabled: boolean; url: string };
/** Плоские права и скоуп из ответа dock; node_name есть, если core знает имя узла. */
type McpRights = {
  logs: boolean;
  metrics: boolean;
  alerts_write: boolean;
  node_scope: NodeScope;
  node_name: string | null;
};
type PersonalToken = McpRights & {
  id: string;
  name: string;
  created_at: number;
  /** null у бессрочного токена. */
  expires_at: number | null;
  last_used_at: number | null;
};
type OAuthGrant = McpRights & {
  id: string;
  client_name: string;
  scope: string;
  created_at: number;
  last_used_at: number | null;
};

function formatTime(value: number | null): string {
  return value === null ? "не использовался" : new Date(value).toLocaleString("ru-RU");
}

const relativeFormat = new Intl.RelativeTimeFormat("ru", { numeric: "auto" });

/** «через 28 дней», «2 часа назад»: крупнейшая единица, в которой разница не меньше 1. */
function relativeTime(value: number, now: number): string {
  const diff = value - now;
  const units: [Intl.RelativeTimeFormatUnit, number][] = [["day", 86_400_000], ["hour", 3_600_000], ["minute", 60_000]];
  for (const [unit, size] of units) {
    if (Math.abs(diff) >= size) return relativeFormat.format(Math.round(diff / size), unit);
  }
  return "только что";
}

function usedText(lastUsed: number | null, now: number): string {
  return lastUsed === null ? "не использовался" : `использован ${relativeTime(lastUsed, now)}`;
}

function tokenSummary(token: PersonalToken, now: number): { text: string; tone: string } {
  const used = usedText(token.last_used_at, now);
  if (token.expires_at === null) return { text: `Бессрочный · ${used}`, tone: "" };
  if (token.expires_at <= now) return { text: `Истёк ${relativeTime(token.expires_at, now)} · ${used}`, tone: "expired" };
  const soon = token.expires_at - now < 3 * 86_400_000;
  return { text: `Истекает ${relativeTime(token.expires_at, now)} · ${used}`, tone: soon ? "soon" : "" };
}

function tokenDetails(token: PersonalToken): string {
  return [
    `Создан ${formatTime(token.created_at)}`,
    token.expires_at === null ? "Срок действия не ограничен" : `Истекает ${formatTime(token.expires_at)}`,
    `Последнее использование: ${formatTime(token.last_used_at)}`,
  ].join("\n");
}

function rights(item: McpRights): string[] {
  return [
    item.logs && "Логи",
    item.metrics && "Метрики",
    item.alerts_write && "Управление алертами",
  ].filter((value): value is string => Boolean(value));
}

function scopeText(item: McpRights): string {
  const { node_scope: scope } = item;
  if (scope.kind === "all") return "Все узлы";
  if (scope.kind === "relay") return `Relay и агенты · ${item.node_name ?? `#${scope.id}`}`;
  return `Агент · ${item.node_name ?? `#${scope.id}`}`;
}

export default function McpAccess() {
  const [settings, setSettings] = useState<McpSettings | null>(null);
  const [tokens, setTokens] = useState<PersonalToken[]>([]);
  const [grants, setGrants] = useState<OAuthGrant[]>([]);
  const [name, setName] = useState("");
  const [expiresInDays, setExpiresInDays] = useState(30);
  const [access, setAccess] = useState<AccessDraft>(defaultAccessDraft);
  const [nodes, setNodes] = useState<NodeLists | null>(null);
  const [secret, setSecret] = useState<string | null>(null);
  const [secretCopied, setSecretCopied] = useState(false);
  const [urlCopied, setUrlCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [now, setNow] = useState(() => Date.now());

  const refresh = async () => {
    const [nextSettings, nextTokens, nextGrants] = await Promise.all([
      api<McpSettings>("GET", "/api/mcp/access"),
      api<{ tokens: PersonalToken[] }>("GET", "/api/mcp/personal-tokens"),
      api<{ grants: OAuthGrant[] }>("GET", "/api/mcp/grants"),
    ]);
    setSettings(nextSettings);
    setTokens(nextTokens.tokens);
    setGrants(nextGrants.grants);
    setNow(Date.now());
  };

  useEffect(() => {
    refresh().catch((reason) => setError(errorText(reason)));
    loadNodeLists().then(setNodes);
  }, []);

  const issue = async (event: FormEvent) => {
    event.preventDefault();
    const permissions = accessBody(access);
    if (!permissions) return;
    setBusy(true);
    setError("");
    setMessage("");
    setSecret(null);
    setSecretCopied(false);
    try {
      const reply = await api<{ token: string }>("POST", "/api/mcp/personal-tokens", {
        name,
        ...permissions,
        ...(expiresInDays === 0 ? { never_expires: true } : { expires_in_days: expiresInDays }),
      });
      setSecret(reply.token);
      setName("");
      setExpiresInDays(30);
      setAccess(defaultAccessDraft);
      setMessage("Токен создан. Сохраните секрет сейчас: после закрытия страницы его нельзя будет восстановить.");
      await refresh();
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  };

  const copy = async (value: string, kind: "url" | "secret") => {
    try {
      await navigator.clipboard.writeText(value);
      setError("");
      if (kind === "url") setUrlCopied(true);
      else setSecretCopied(true);
    } catch {
      setError(kind === "url"
        ? "Не удалось скопировать адрес. Выделите и скопируйте его вручную."
        : "Не удалось скопировать секрет. Выделите и скопируйте его вручную.");
    }
  };

  const revokeToken = async (token: PersonalToken) => {
    if (!window.confirm(`Отозвать персональный токен «${token.name}»?`)) return;
    setBusy(true);
    setError("");
    try {
      await api("DELETE", `/api/mcp/personal-tokens/${encodeURIComponent(token.id)}`);
      await refresh();
      setMessage(`Токен «${token.name}» отозван.`);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  };

  const revokeGrant = async (grant: OAuthGrant) => {
    if (!window.confirm(`Отозвать доступ «${grant.client_name}»?`)) return;
    setBusy(true);
    setError("");
    try {
      await api("DELETE", `/api/mcp/grants/${encodeURIComponent(grant.id)}`);
      await refresh();
      setMessage(`Доступ «${grant.client_name}» отозван.`);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="mcp-access">
      <header className="mcp-access-heading">
        <h1>MCP-доступ</h1>
        <p className="page-note">Подключайте внешние MCP-клиенты к логам и метрикам Lognara и отзывайте доступы.</p>
      </header>

      <article className="mcp-card mcp-server-card">
        <div>
          <h2>Сервер MCP</h2>
          <p className="page-note">Используйте этот адрес при настройке MCP-клиента.</p>
        </div>
        <div className="mcp-server-address">
          <code>{settings?.url ?? "Загружаем адрес…"}</code>
          <button type="button" className="quiet" disabled={!settings?.url} onClick={() => settings && void copy(settings.url, "url")}>
            {urlCopied ? "Адрес скопирован" : "Скопировать адрес"}
          </button>
        </div>
        {settings && (
          <div className="mcp-server-status">
            <span className={`mcp-status-chip ${settings.enabled ? "enabled" : "disabled"}`}>
              {settings.enabled ? "Включён" : "Выключен"}
            </span>
            <p className="page-note">
              {settings.enabled
                ? "Сервер принимает подключения с персональными токенами и OAuth."
                : "Сервер выключен. Администратору нужно включить LOGNARA_DOCK_MCP_ENABLED и опубликовать адрес через HTTPS nginx."}
            </p>
          </div>
        )}
        <p className="muted">Доступ к логам и метрикам выдаётся отдельно. Токены браузерной сессии не подходят для MCP.</p>
      </article>

      <div className="mcp-grid">
        <article className="mcp-card">
          <header className="mcp-card-heading">
            <h2>Выпустить персональный токен</h2>
            <p className="page-note">Выберите права, область данных и срок действия токена.</p>
          </header>
          <form className="mcp-token-form" onSubmit={issue}>
            <label htmlFor="mcp-token-name">
              Название
              <input id="mcp-token-name" value={name} onChange={(event) => setName(event.target.value)} maxLength={80} placeholder="Codex на рабочем компьютере" />
            </label>
            <AccessFields draft={access} onChange={setAccess} nodes={nodes} appearance="cards" />
            <label htmlFor="mcp-token-expiry">
              Срок действия
              <select id="mcp-token-expiry" value={expiresInDays} onChange={(event) => setExpiresInDays(Number(event.target.value))}>
                <option value={7}>7 дней</option>
                <option value={30}>30 дней</option>
                <option value={90}>90 дней</option>
                <option value={0}>Без ограничения</option>
              </select>
            </label>
            {expiresInDays === 0 && <p className="muted">Бессрочный токен действует, пока его не отзовут.</p>}
            <button type="submit" className="solid" disabled={busy || !settings?.enabled || !name.trim() || !accessBody(access)}>
              Выпустить токен
            </button>
          </form>

          {secret && (
            <div className="mcp-secret" role="region" aria-label="Выданный токен">
              <div>
                <strong>Сохраните секрет сейчас</strong>
                <p>Токен показывается один раз и не появится в списке после закрытия этой страницы.</p>
              </div>
              <label htmlFor="mcp-secret">Скопируйте секрет сейчас</label>
              <textarea id="mcp-secret" readOnly value={secret} rows={2} />
              <button type="button" onClick={() => void copy(secret, "secret")}>
                {secretCopied ? "Скопировано" : "Скопировать токен"}
              </button>
            </div>
          )}
        </article>

        <article className="mcp-card">
          <header className="mcp-card-heading">
            <h2>Персональные токены</h2>
            <p className="page-note">Токены, выпущенные для этого аккаунта.</p>
          </header>
          <ul className="mcp-list">
            {tokens.map((token) => {
              const summary = tokenSummary(token, now);
              return <li key={token.id}>
                <div className="mcp-record">
                  <strong>{token.name}</strong>
                  <div className="mcp-chips">
                    {rights(token).map((right) => <span className="mcp-chip" key={right}>{right}</span>)}
                    <span className="mcp-chip scope">{scopeText(token)}</span>
                  </div>
                  <p className={`mcp-record-meta ${summary.tone}`} title={tokenDetails(token)}>{summary.text}</p>
                </div>
                <button type="button" className="quiet" disabled={busy} onClick={() => void revokeToken(token)}>Отозвать</button>
              </li>;
            })}
            {tokens.length === 0 && <li className="muted">Персональных токенов нет</li>}
          </ul>
        </article>
      </div>

      <article className="mcp-card">
        <header className="mcp-card-heading">
          <h2>Доступы OAuth</h2>
          <p className="page-note">Согласия, выданные MCP-клиентам через OAuth.</p>
        </header>
        <ul className="mcp-list">
          {grants.map((grant) => (
            <li key={grant.id}>
              <div className="mcp-record">
                <strong>{grant.client_name}</strong>
                <div className="mcp-chips">
                  {rights(grant).map((right) => <span className="mcp-chip" key={right}>{right}</span>)}
                  <span className="mcp-chip scope">{scopeText(grant)}</span>
                </div>
                <p className="mcp-record-meta" title={[
                  `Разрешение: ${grant.scope}`,
                  `Выдано ${formatTime(grant.created_at)}`,
                  `Последнее использование: ${formatTime(grant.last_used_at)}`,
                ].join("\n")}>
                  Выдано {relativeTime(grant.created_at, now)} · {usedText(grant.last_used_at, now)}
                </p>
              </div>
              <button type="button" className="quiet" disabled={busy} onClick={() => void revokeGrant(grant)}>Отозвать</button>
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
