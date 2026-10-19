import { type FormEvent, useEffect, useState } from "react";
import { api, ApiError, type Agent, errorText, type Relay } from "../api";

/** Токен, который core показывает один раз. */
interface Issued {
  kind: "relay" | "agent";
  name: string;
  token: string;
  envLine: string;
  rotated?: boolean;
}

export default function Access({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [relays, setRelays] = useState<Relay[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [relayName, setRelayName] = useState("");
  const [agentName, setAgentName] = useState("");
  const [relayId, setRelayId] = useState("");
  const [issued, setIssued] = useState<Issued | null>(null);
  const [copyStatus, setCopyStatus] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const load = () =>
    Promise.all([
      api<{ relays: Relay[] }>("GET", "/api/relays"),
      api<{ agents: Agent[] }>("GET", "/api/agents"),
    ]).then(([relayReply, agentReply]) => {
      setRelays(relayReply.relays);
      setAgents(agentReply.agents);
    });

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        onUnauthorized();
      } else {
        setError(errorText(e));
      }
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    run(async () => undefined);
  }, []);

  const agentsByRelay = new Map<number, Agent[]>();
  for (const agent of agents) {
    const group = agentsByRelay.get(agent.relay_id) ?? [];
    group.push(agent);
    agentsByRelay.set(agent.relay_id, group);
  }
  // Пока relay не выбран явно, предлагается первый из списка.
  const selectedRelay = relays.some((relay) => String(relay.id) === relayId)
    ? relayId
    : String(relays[0]?.id ?? "");

  const addRelay = (event: FormEvent) => {
    event.preventDefault();
    run(async () => {
      const reply = await api<{ relay: Relay; token: string }>("POST", "/api/relays", {
        name: relayName,
      });
      setIssued({
        kind: "relay",
        name: reply.relay.name,
        token: reply.token,
        envLine: `LOGNARA_RELAY_CORE_TOKEN=${reply.token}`,
      });
      setCopyStatus("");
      setRelayId(String(reply.relay.id));
      setRelayName("");
    });
  };

  const addAgent = (event: FormEvent) => {
    event.preventDefault();
    run(async () => {
      const reply = await api<{ agent: Agent; token: string }>("POST", "/api/agents", {
        name: agentName,
        relay_id: Number(selectedRelay),
      });
      setIssued({
        kind: "agent",
        name: reply.agent.name,
        token: reply.token,
        envLine: `LOGNARA_AGENT_TOKEN=${reply.token}`,
      });
      setCopyStatus("");
      setAgentName("");
    });
  };

  const rotate = (agent: Agent) => {
    if (!window.confirm(`Перевыпустить токен агента «${agent.name}»? Старый токен перестанет работать после синхронизации relay. Новый токен нужно установить в конфигурации агента.`)) return;
    run(async () => {
      const reply = await api<{ agent: Agent; token: string }>("POST", `/api/agents/${agent.id}/rotate-token`);
      setIssued({
        kind: "agent",
        name: reply.agent.name,
        token: reply.token,
        envLine: `LOGNARA_AGENT_TOKEN=${reply.token}`,
        rotated: true,
      });
      setCopyStatus("");
    });
  };

  const copy = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopyStatus(`${label} скопирован${label === "Токен" ? "" : "а"}.`);
    } catch {
      setCopyStatus("Не удалось скопировать. Выделите нужный текст в поле и скопируйте вручную.");
    }
  };

  const remove = (kind: "relays" | "agents", id: number, name: string) => {
    if (!window.confirm(`Удалить «${name}»? Его токен перестанет работать.`)) return;
    run(async () => {
      await api("DELETE", `/api/${kind}/${id}`);
    });
  };

  return (
    <section className="access">
      <h2>Подключения</h2>
      <p className="muted">Создайте relay, затем подключите к нему агентов для отправки логов.</p>
      <div className="access-create">
        <form onSubmit={addRelay} aria-label="Создать relay">
          <h3>Создать relay</h3>
          <label>
            Имя нового relay
            <input value={relayName} onChange={(e) => setRelayName(e.target.value)} placeholder="main" />
          </label>
          <button type="submit" disabled={busy || issued !== null || !relayName.trim()}>Добавить relay</button>
        </form>
        <form onSubmit={addAgent} aria-label="Создать агента">
          <h3>Создать агента</h3>
          <div className="access-agent-fields">
            <label>
              Relay агента
              <select value={selectedRelay} onChange={(e) => setRelayId(e.target.value)} disabled={relays.length === 0}>
                {relays.length === 0 && <option value="">Сначала создайте relay</option>}
                {relays.map((relay) => <option key={relay.id} value={relay.id}>{relay.name}</option>)}
              </select>
            </label>
            <label>
              Имя нового агента
              <input value={agentName} onChange={(e) => setAgentName(e.target.value)} placeholder="api" />
            </label>
          </div>
          <button type="submit" disabled={busy || issued !== null || !agentName.trim() || !selectedRelay}>Добавить агента</button>
        </form>
      </div>
      {issued && (
        <div className="token" role="region" aria-label="Выданный токен">
          <p>
            <strong>{issued.rotated ? `Токен агента «${issued.name}» перевыпущен.` : `${issued.kind === "relay" ? "Relay" : "Агент"} «${issued.name}» создан.`}</strong>{" "}
            Токен показан только сейчас и больше не покажется: добавьте строку в .env и сохраните её.
          </p>
          {issued.rotated && <p className="muted">Обновите конфигурацию агента. Замена применяется после успешной синхронизации relay с core, обычно в пределах 10 секунд.</p>}
          <label>
            Строка для .env
            <textarea readOnly value={issued.envLine} rows={3} spellCheck={false} />
          </label>
          <div className="access-actions">
            <button type="button" onClick={() => copy(issued.envLine, "Строка .env")}>Скопировать строку .env</button>
            <button type="button" onClick={() => copy(issued.token, "Токен")}>Скопировать токен</button>
            <button type="button" onClick={() => { setIssued(null); setCopyStatus(""); }}>Я сохранил токен</button>
          </div>
          <p className="access-copy-status" role="status">{copyStatus}</p>
        </div>
      )}
      {error && <p className="error">{error}</p>}

      <div className="access-list-heading">
        <h3>Relay и агенты</h3>
        <span className="muted">Relay: {relays.length} · Агенты: {agents.length}</span>
      </div>
      <div className="access-relays">
        {relays.map((relay) => {
          const relayAgents = agentsByRelay.get(relay.id) ?? [];
          return (
            <section className="access-relay" key={relay.id} aria-label={`Relay ${relay.name}`}>
              <header className="access-relay-header">
                <div>
                  <h3>{relay.name}</h3>
                  <span className="muted">Создан {new Date(relay.created_at).toLocaleString()} · Агентов: {relayAgents.length}</span>
                </div>
                <button type="button" className="quiet" onClick={() => remove("relays", relay.id, relay.name)} disabled={busy}>Удалить relay</button>
              </header>
              <div className="access-agent-grid">
                {relayAgents.map((agent) => (
                  <article className="access-agent" key={agent.id} aria-label={`Агент ${agent.name}`}>
                    <h4>{agent.name}</h4>
                    <p className="muted">Создан {new Date(agent.created_at).toLocaleString()}</p>
                    <div className="access-actions">
                      <button type="button" onClick={() => rotate(agent)} disabled={busy || issued !== null}>Перевыпустить токен</button>
                      <button type="button" className="quiet" onClick={() => remove("agents", agent.id, agent.name)} disabled={busy}>Удалить</button>
                    </div>
                  </article>
                ))}
              </div>
              {relayAgents.length === 0 && <p className="empty">У этого relay пока нет агентов.</p>}
            </section>
          );
        })}
        {relays.length === 0 && !busy && <p className="empty">Relay пока нет. Создайте первый relay в форме выше.</p>}
      </div>
    </section>
  );
}
