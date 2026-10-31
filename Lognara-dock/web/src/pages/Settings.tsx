import { type FormEvent, useEffect, useState } from "react";
import { api, errorText, type PasskeyInfo } from "../api";
import { createPasskey } from "../webauthn";
import MetricAlertSettings from "./MetricAlertSettings";

function formatTime(ms: number | null): string {
  return ms === null ? "не использовался" : new Date(ms).toLocaleString();
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
    <form onSubmit={submit}>
      <h2>Пароль</h2>
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
      {message && <p className="ok">{message}</p>}
      {error && <p className="error">{error}</p>}
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
    <form onSubmit={add}>
      <h2>Passkey</h2>
      <ul className="passkeys">
        {passkeys.map((key) => (
          <li key={key.id}>
            <strong>{key.name}</strong>
            <span className="muted">
              добавлен {formatTime(key.created_at)}, вход {formatTime(key.last_used_at)}
            </span>
            <button type="button" onClick={() => remove(key.id)} disabled={busy}>
              Удалить
            </button>
          </li>
        ))}
        {passkeys.length === 0 && <li className="muted">Ключей нет</li>}
      </ul>
      <label>
        Имя нового ключа
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="MacBook" />
      </label>
      <label>
        Текущий пароль (для добавления и удаления)
        <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
      </label>
      <button type="submit" disabled={busy || !name}>
        Добавить passkey
      </button>
      {error && <p className="error">{error}</p>}
    </form>
  );
}

export default function Settings({
  onChange,
  onUnauthorized,
}: {
  onChange: () => void;
  onUnauthorized: () => void;
}) {
  return (
    <section className="settings">
      <PasswordForm />
      <Passkeys onChange={onChange} />
      <MetricAlertSettings onUnauthorized={onUnauthorized} />
    </section>
  );
}
