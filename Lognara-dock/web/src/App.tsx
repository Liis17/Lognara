import { useEffect, useState } from "react";
import { Navigate, NavLink, Route, Routes, useNavigate } from "react-router";
import { api, type Me } from "./api";
import Access from "./pages/Access";
import Login from "./pages/Login";
import Logs from "./pages/Logs";
import Metrics from "./pages/Metrics";
import Settings from "./pages/Settings";

export default function App() {
  // undefined: ещё проверяем сессию, null: не вошли.
  const [me, setMe] = useState<Me | null | undefined>(undefined);
  const navigate = useNavigate();

  const refresh = () =>
    api<Me>("GET", "/api/me")
      .then(setMe)
      .catch(() => setMe(null));

  useEffect(() => {
    refresh();
  }, []);

  if (me === undefined) return null;

  if (me === null) {
    return (
      <Routes>
        <Route path="/login" element={<Login onLogin={refresh} />} />
        <Route path="*" element={<Navigate to="/login" replace />} />
      </Routes>
    );
  }

  const logout = async () => {
    await api("POST", "/api/auth/logout").catch(() => undefined);
    setMe(null);
    navigate("/login");
  };

  return (
    <div className="app">
      <header>
        <strong>Lognara</strong>
        <nav>
          <NavLink to="/logs">Логи</NavLink>
          <NavLink to="/metrics">Метрики</NavLink>
          <NavLink to="/access">Подключения</NavLink>
          <NavLink to="/settings">Настройки</NavLink>
        </nav>
        <span className="user">{me.username}</span>
        <button onClick={logout}>Выйти</button>
      </header>
      <main>
        <Routes>
          <Route path="/logs" element={<Logs onUnauthorized={() => setMe(null)} />} />
          <Route
            path="/metrics"
            element={<Metrics onUnauthorized={() => setMe(null)} />}
          />
          <Route path="/access" element={<Access onUnauthorized={() => setMe(null)} />} />
          <Route path="/settings" element={<Settings onChange={refresh} />} />
          <Route path="*" element={<Navigate to="/logs" replace />} />
        </Routes>
      </main>
    </div>
  );
}
