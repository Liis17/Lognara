import { Component, type ReactNode, Suspense, lazy, useEffect, useRef, useState } from "react";
import { Navigate, NavLink, Route, Routes, useLocation, useNavigate } from "react-router";
import { api, type Me } from "./api";

// Разделы грузятся отдельными чанками при первом переходе.
const Access = lazy(() => import("./pages/Access"));
const Analytics = lazy(() => import("./pages/Analytics"));
const Errors = lazy(() => import("./pages/Errors"));
const Infrastructure = lazy(() => import("./pages/Infrastructure"));
const InfrastructureTree = lazy(() => import("./pages/InfrastructureTree"));
const Login = lazy(() => import("./pages/Login"));
const Logs = lazy(() => import("./pages/Logs"));
const Metrics = lazy(() => import("./pages/Metrics"));
const MetricDetail = lazy(() => import("./pages/MetricDetail"));
const McpAccess = lazy(() => import("./pages/McpAccess"));
const OAuthConsent = lazy(() => import("./pages/OAuthConsent"));
const Settings = lazy(() => import("./pages/Settings"));

const loadingPage = <div className="session-state" role="status">Загружаем раздел…</div>;

// После обновления dock открытая вкладка может запросить уже удалённый чанк.
class ChunkBoundary extends Component<{ children: ReactNode; resetKey: string }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  componentDidUpdate(previous: { resetKey: string }) {
    if (this.state.failed && previous.resetKey !== this.props.resetKey) this.setState({ failed: false });
  }
  render() {
    if (!this.state.failed) return this.props.children;
    return <div className="session-state" role="alert">
      <p>Не удалось загрузить раздел. Возможно, Dock обновился.</p>
      <button type="button" className="button solid" onClick={() => window.location.reload()}>Перезагрузить страницу</button>
    </div>;
  }
}

export default function App() {
  // undefined: ещё проверяем сессию, null: не вошли.
  const [me, setMe] = useState<Me | null | undefined>(undefined);
  const navigate = useNavigate();
  const { pathname, search } = useLocation();
  const logoutDialog = useRef<HTMLDialogElement>(null);

  const refresh = () =>
    api<Me>("GET", "/api/me")
      .then(setMe)
      .catch(() => setMe(null));

  useEffect(() => {
    refresh();
  }, []);

  if (me === undefined) {
    return <div className="session-state" role="status">Проверяем сессию…</div>;
  }

  if (me === null) {
    return (
      <ChunkBoundary resetKey={pathname}>
        <Suspense fallback={loadingPage}>
          <Routes>
            <Route path="/login" element={<Login onLogin={refresh} />} />
            <Route
              path="*"
              element={
                <Navigate
                  to={pathname === "/oauth/consent" ? `/login?return_to=${encodeURIComponent(`${pathname}${search}`)}` : "/login"}
                  replace
                />
              }
            />
          </Routes>
        </Suspense>
      </ChunkBoundary>
    );
  }

  const logout = async () => {
    logoutDialog.current?.close();
    await api("POST", "/api/auth/logout").catch(() => undefined);
    setMe(null);
    navigate("/login");
  };

  return (
    <div className="app">
      <header className="topbar">
        <NavLink className="brand" to="/logs" aria-label="Lognara Dock, логи">
          lognara<span><b>//</b>dock</span>
        </NavLink>
        <nav aria-label="Разделы">
          <NavLink to="/logs">Логи</NavLink>
          <NavLink to="/errors">Ошибки</NavLink>
          <NavLink to="/analytics">Аналитика</NavLink>
          <NavLink to="/infrastructure">Инфраструктура</NavLink>
          <NavLink to="/metrics">Метрики</NavLink>
          <NavLink to="/access">Подключения</NavLink>
          <NavLink to="/mcp-access">MCP-доступ</NavLink>
          <NavLink to="/settings">Настройки</NavLink>
        </nav>
        <div className="user">
          <img src="/favicon.png" alt="" width="28" height="28" />
          <span>{me.username}</span>
        </div>
        <button className="quiet" onClick={() => logoutDialog.current?.showModal()}>Выйти</button>
      </header>
      <dialog ref={logoutDialog} className="confirm-dialog" aria-labelledby="logout-title">
        <h2 id="logout-title">Выйти из Lognara?</h2>
        <p>Вы сможете снова войти в любой момент.</p>
        <div className="confirm-actions">
          <button type="button" className="quiet" onClick={() => logoutDialog.current?.close()}>Отмена</button>
          <button type="button" className="solid" onClick={logout}>Выйти</button>
        </div>
      </dialog>
      <main>
        <ChunkBoundary resetKey={pathname}>
          <Suspense fallback={loadingPage}>
            <Routes>
              <Route path="/logs" element={<Logs onUnauthorized={() => setMe(null)} />} />
              <Route path="/errors" element={<Errors onUnauthorized={() => setMe(null)} />} />
              <Route path="/analytics" element={<Analytics onUnauthorized={() => setMe(null)} />} />
              <Route path="/infrastructure" element={<Infrastructure onUnauthorized={() => setMe(null)} />} />
              <Route path="/infrastructure/tree" element={<InfrastructureTree onUnauthorized={() => setMe(null)} />} />
              <Route
                path="/metrics"
                element={<Metrics onUnauthorized={() => setMe(null)} />}
              />
              <Route path="/metrics/detail" element={<MetricDetail onUnauthorized={() => setMe(null)} />} />
              <Route path="/access" element={<Access onUnauthorized={() => setMe(null)} />} />
              <Route path="/mcp-access" element={<McpAccess />} />
              <Route path="/oauth/consent" element={<OAuthConsent />} />
              <Route path="/settings" element={<Settings onChange={refresh} />} />
              <Route path="*" element={<Navigate to="/logs" replace />} />
            </Routes>
          </Suspense>
        </ChunkBoundary>
      </main>
    </div>
  );
}
