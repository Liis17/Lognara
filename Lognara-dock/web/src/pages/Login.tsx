import { type CSSProperties, type FormEvent, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { api, errorText } from "../api";
import { getPasskey } from "../webauthn";

const between = (a: number, b: number) => a + Math.random() * (b - a);

// Случайные размер, позиция и траектория фоновых пятен; считаются один раз на монтирование.
const makeBlobs = () =>
  [1, 2, 3, 4].map((i) => {
    const size = Math.round(between(280, 560));
    return {
      key: i,
      style: {
        width: size,
        height: size,
        left: `${Math.round(between(-20, 80))}%`,
        top: `${Math.round(between(-20, 80))}%`,
        "--dx": `${Math.round(between(-300, 300))}px`,
        "--dy": `${Math.round(between(-240, 240))}px`,
        "--sc": between(0.85, 1.25).toFixed(2),
        animationDuration: `${Math.round(between(28, 48))}s`,
        animationDelay: `${-Math.round(between(0, 30))}s`,
      } as CSSProperties,
    };
  });

export default function Login({ onLogin }: { onLogin: () => Promise<void> }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [blobs] = useState(makeBlobs);
  const navigate = useNavigate();
  const location = useLocation();

  const finish = async () => {
    await onLogin();
    const returnTo = new URLSearchParams(location.search).get("return_to");
    if (returnTo && ["/oauth/authorize", "/oauth/consent"].some((path) => returnTo.startsWith(`${path}?`))) {
      const destination = new URL(returnTo, window.location.origin);
      if (destination.origin === window.location.origin && !returnTo.startsWith("//")) {
        window.location.assign(`${destination.pathname}${destination.search}`);
        return;
      }
    }
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
    <div className="login-page">
      {blobs.map((blob) => (
        <div key={blob.key} className={`login-blob b${blob.key}`} style={blob.style} aria-hidden="true" />
      ))}
      <main className="login">
        <header className="login-brand">
          <img src="/favicon.png" alt="" width="44" height="44" />
          <div>
            <h1>Lognara</h1>
            <p>Система сбора логов</p>
          </div>
        </header>

        <form onSubmit={submit}>
          <div className="login-field">
            <label htmlFor="login-username">Логин</label>
            <input
              id="login-username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              autoComplete="username"
              placeholder="Введите логин"
              autoFocus
            />
          </div>
          <div className="login-field">
            <label htmlFor="login-password">Пароль</label>
            <div className="login-password">
              <input
                id="login-password"
                type={showPassword ? "text" : "password"}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="current-password"
                placeholder="Введите пароль"
              />
              <button
                type="button"
                aria-label={showPassword ? "Скрыть пароль" : "Показать пароль"}
                aria-pressed={showPassword}
                onClick={() => setShowPassword((value) => !value)}
              >
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" />
                  <circle cx="12" cy="12" r="3" />
                  {showPassword && <path d="M3 3l18 18" />}
                </svg>
              </button>
            </div>
          </div>
          <button type="submit" disabled={busy || !username || !password}>
            Войти
          </button>
        </form>

        {error && <p className="error" role="alert">{error}</p>}

        <div className="login-divider"><span>или</span></div>

        <button type="button" className="login-passkey" onClick={passkey} disabled={busy}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <circle cx="8" cy="15" r="4" />
            <path d="M10.85 12.15 19 4" />
            <path d="M18 5l3 3" />
            <path d="M15 8l2 2" />
          </svg>
          Войти по passkey
        </button>

        <p className="login-note">Нет учётной записи? Обратитесь к администратору</p>
      </main>
    </div>
  );
}
