import { type FormEvent, useState } from "react";
import { useNavigate } from "react-router";
import { api, errorText } from "../api";
import { getPasskey } from "../webauthn";

export default function Login({ onLogin }: { onLogin: () => Promise<void> }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const navigate = useNavigate();

  const finish = async () => {
    await onLogin();
    navigate("/logs", { replace: true });
  };

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
      await finish();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    run(async () => {
      await api("POST", "/api/auth/login", { username, password });
    });
  };

  const passkey = () =>
    run(async () => {
      const options = await api("POST", "/api/auth/passkey/start");
      const credential = await getPasskey(options);
      await api("POST", "/api/auth/passkey/finish", credential);
    });

  return (
    <div className="login">
      <h1>Lognara</h1>
      <form onSubmit={submit}>
        <label>
          Логин
          <input value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" autoFocus />
        </label>
        <label>
          Пароль
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
          />
        </label>
        <button type="submit" disabled={busy || !username || !password}>
          Войти
        </button>
      </form>
      <button onClick={passkey} disabled={busy}>
        Войти по passkey
      </button>
      {error && <p className="error">{error}</p>}
    </div>
  );
}
