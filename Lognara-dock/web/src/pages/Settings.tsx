import { type FormEvent, useEffect, useState } from "react";
import { NavLink, useLocation, useNavigate } from "react-router";
import { api, errorText, type PasskeyInfo } from "../api";
import { createPasskey } from "../webauthn";
import McpAccess from "./McpAccess";

type SettingsTab = "account" | "passkeys" | "mcp";

const tabs: { id: SettingsTab; label: string }[] = [
  { id: "account", label: "Аккаунт" },
  { id: "passkeys", label: "Вход и passkey" },
  { id: "mcp", label: "MCP-доступ" },
];

function formatTime(ms: number | null): string {
  return ms === null ? "не использовался" : new Date(ms).toLocaleString("ru-RU");
}

function PasswordForm() {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [repeat, setRepeat] = useState("");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setMessage("");
    setError("");
    if (next !== repeat) {
      setError("Новые пароли не совпадают");
      return;
    }
    try {
      await api("POST", "/api/account/password", { current, new: next });
      setCurrent("");
      setNext("");
      setRepeat("");
      setMessage("Пароль изменён, остальные сессии завершены");
    } catch (e) {
      setError(errorText(e));
    }
  };

  return (
    <form className="settings-form" onSubmit={submit}>
      <label>
        Текущий пароль
        <input type="password" value={current} onChange={(e) => setCurrent(e.target.value)} autoComplete="current-password" />
      </label>
      <label>
        Новый пароль
        <input type="password" value={next} onChange={(e) => setNext(e.target.value)} autoComplete="new-password" />
      </label>
      <label>
        Повторите новый пароль
        <input type="password" value={repeat} onChange={(e) => setRepeat(e.target.value)} autoComplete="new-password" />
      </label>
      <button type="submit" disabled={!current || !next}>
        Сменить пароль
      </button>
      {message && <p className="ok" role="status">{message}</p>}
      {error && <p className="error" role="alert">{error}</p>}
    </form>
  );
}

function Passkeys({ onChange }: { onChange: () => void }) {
  const [passkeys, setPasskeys] = useState<PasskeyInfo[]>([]);
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const load = () =>
    api<{ passkeys: PasskeyInfo[] }>("GET", "/api/account/passkeys")
      .then((reply) => setPasskeys(reply.passkeys))
      .catch((e) => setError(errorText(e)));

  useEffect(() => {
    load();
  }, []);

  const run = async (action: () => Promise<void>) => {
    if (!password) {
      setError("Введите текущий пароль");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
      onChange();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const add = (event: FormEvent) => {
    event.preventDefault();
    run(async () => {
      const options = await api("POST", "/api/account/passkeys/start", { name, password });
      const credential = await createPasskey(options);
      await api("POST", "/api/account/passkeys/finish", credential);
      setName("");
    });
  };

  const remove = (id: number) =>
    run(async () => {
      await api("DELETE", `/api/account/passkeys/${id}`, { password });
    });

  return (
    <div className="passkey-settings">
      <ul className="passkeys">
        {passkeys.map((key) => (
          <li key={key.id}>
            <div className="passkey-info">
              <strong>{key.name}</strong>
              <span className="muted">Добавлен {formatTime(key.created_at)}</span>
              <span className="muted">Последний вход {formatTime(key.last_used_at)}</span>
            </div>
            <button type="button" className="quiet" onClick={() => remove(key.id)} disabled={busy || !password}>
              Удалить
            </button>
          </li>
        ))}
        {passkeys.length === 0 && <li className="muted">Ключей нет</li>}
      </ul>
      <form className="settings-form" onSubmit={add}>
        <label>
          Имя нового ключа
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="MacBook" />
        </label>
        <label>
          Текущий пароль
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
        </label>
        <button type="submit" disabled={busy || !name.trim() || !password}>
          Добавить passkey
        </button>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </div>
  );
}

export default function Settings({ onChange }: { onChange: () => void }) {
  const location = useLocation();
  const navigate = useNavigate();
  const pathTab = location.pathname.split("/")[2];
  const validTab = tabs.some((tab) => tab.id === pathTab);
  const activeTab: SettingsTab = validTab ? pathTab as SettingsTab : "account";

  useEffect(() => {
    if (location.pathname !== `/settings/${activeTab}`) {
      navigate(`/settings/account${location.search}${location.hash}`, { replace: true });
    }
  }, [activeTab, location.hash, location.pathname, location.search, navigate]);

  return (
    <section className="settings-page">
      <div className="settings-layout">
        <nav className="settings-sidebar" aria-label="Настройки">
          {tabs.map((tab) => (
            <NavLink
              key={tab.id}
              to={`/settings/${tab.id}${location.search}${location.hash}`}
              end
              aria-current={activeTab === tab.id ? "page" : undefined}
            >
              {tab.label}
            </NavLink>
          ))}
        </nav>

        <div className="settings-content">
          {activeTab === "account" && (
            <section className="settings-section-card">
              <header>
                <h1>Аккаунт</h1>
                <p>Обновите пароль для этого аккаунта. После смены остальные сессии завершатся.</p>
              </header>
              <div className="settings-section-body">
                <h2>Пароль</h2>
                <p className="muted">Для подтверждения укажите текущий пароль и введите новый дважды.</p>
                <PasswordForm />
              </div>
            </section>
          )}

          {activeTab === "passkeys" && (
            <section className="settings-section-card">
              <header>
                <h1>Вход и passkey</h1>
                <p>Управляйте ключами входа, сохранёнными на ваших устройствах.</p>
              </header>
              <div className="settings-section-body">
                <h2>Passkey</h2>
                <p className="muted">Добавление и удаление ключа подтверждается текущим паролем.</p>
                <Passkeys onChange={onChange} />
              </div>
            </section>
          )}

          {activeTab === "mcp" && <McpAccess />}
        </div>
      </div>
    </section>
  );
}
