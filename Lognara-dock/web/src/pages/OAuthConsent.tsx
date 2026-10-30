import { useEffect, useState } from "react";
import { useLocation } from "react-router";
import { api, errorText } from "../api";

type ConsentDetails = { client_name: string; scope: string; resource: string };

function checkedRedirect(value: string): string {
  const target = new URL(value);
  const validPath = target.pathname === "/connector_platform_oauth_redirect"
    || /^\/connector\/oauth\/[A-Za-z0-9_-]+$/u.test(target.pathname);
  if (target.protocol !== "https:" || target.hostname !== "chatgpt.com" || target.port
    || target.username || target.password || !validPath
    || !target.searchParams.has("state") || !target.searchParams.has("iss")) {
    throw new Error("Некорректный адрес возврата от OAuth-сервера");
  }
  return target.href;
}

export default function OAuthConsent() {
  const location = useLocation();
  const requestId = new URLSearchParams(location.search).get("request");
  const [details, setDetails] = useState<ConsentDetails | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!requestId) {
      setError("Запрос на доступ не найден. Начните подключение заново.");
      return;
    }
    api<ConsentDetails>("GET", `/api/oauth/consent/${encodeURIComponent(requestId)}`)
      .then(setDetails)
      .catch((reason) => setError(errorText(reason)));
  }, [requestId]);

  const decide = async (allow: boolean) => {
    if (!requestId) return;
    setBusy(true);
    setError("");
    try {
      const reply = await api<{ redirect_to: string }>("POST", `/api/oauth/consent/${encodeURIComponent(requestId)}`, { allow });
      window.location.assign(checkedRedirect(reply.redirect_to));
    } catch (reason) {
      setError(errorText(reason));
      setBusy(false);
    }
  };

  return (
    <section className="oauth-consent">
      <section className="mcp-card">
        <p className="eyebrow">Lognara Dock</p>
        <h1>Разрешить подключение?</h1>
        {details ? (
          <>
            <p><strong>{details.client_name}</strong> запрашивает доступ к Lognara.</p>
            <dl>
              <dt>Разрешение</dt>
              <dd>{details.scope}</dd>
              <dt>Сервер</dt>
              <dd><code>{details.resource}</code></dd>
            </dl>
            <p className="page-note">Подключение сможет искать логи, читать trace и просматривать метрики. MCP-инструменты доступны только для чтения; текст логов не маскируется.</p>
            <div className="oauth-actions">
              <button type="button" className="quiet" onClick={() => decide(false)} disabled={busy}>Отклонить</button>
              <button type="button" className="solid" onClick={() => decide(true)} disabled={busy}>Разрешить</button>
            </div>
          </>
        ) : !error ? <p role="status">Загружаем запрос…</p> : null}
        {error && <p className="error" role="alert">{error}</p>}
      </section>
    </section>
  );
}
