import { type FormEvent, useEffect, useState } from "react";
import { api, ApiError, type Agent, errorText, type Relay } from "../api";

/** Токен, который core показывает один раз. */
interface Issued {
  kind: "relay" | "agent";
  name: string;
  envLine: string;
}

export default function Access({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [relays, setRelays] = useState<Relay[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [relayName, setRelayName] = useState("");
  const [agentName, setAgentName] = useState("");
  const [relayId, setRelayId] = useState("");
  const [issued, setIssued] = useState<Issued | null>(null);
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

  const relayNames = new Map(relays.map((relay) => [relay.id, relay.name]));
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
        envLine: `LOGNARA_RELAY_CORE_TOKEN=${reply.token}`,
      });
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
        envLine: `LOGNARA_AGENT_TOKEN=${reply.token}`,
      });
      setAgentName("");
    });
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
      {issued && (
        <div className="token">
          <p>
            <strong>{issued.kind === "relay" ? "Relay" : "Агент"} «{issued.name}» создан.</strong>{" "}
            Токен показан только сейчас и больше не покажется: добавьте строку в .env и сохраните её.
          </p>
          <pre>{issued.envLine}</pre>
          <button type="button" onClick={() => setIssued(null)}>
            Я сохранил токен
          </button>
        </div>
      )}
      {error && <p className="error">{error}</p>}

      <h3>Relay</h3>
      <ul>
        {relays.map((relay) => (
          <li key={relay.id}>
            <strong>{relay.name}</strong>
            <span className="muted">создан {new Date(relay.created_at).toLocaleString()}</span>
            <button type="button" onClick={() => remove("relays", relay.id, relay.name)} disabled={busy}>
              Удалить
            </button>
          </li>
        ))}
        {relays.length === 0 && <li className="muted">Relay нет</li>}
      </ul>
      <form onSubmit={addRelay}>
        <label>
          Имя нового relay
          <input value={relayName} onChange={(e) => setRelayName(e.target.value)} placeholder="main" />
        </label>
        <button type="submit" disabled={busy || issued !== null || !relayName.trim()}>
          Добавить relay
        </button>
      </form>

      <h3>Агенты</h3>
      <ul>
        {agents.map((agent) => (
          <li key={agent.id}>
            <strong>{agent.name}</strong>
            <span className="muted">
              relay {relayNames.get(agent.relay_id) ?? agent.relay_id}, создан{" "}
              {new Date(agent.created_at).toLocaleString()}
            </span>
            <button type="button" onClick={() => remove("agents", agent.id, agent.name)} disabled={busy}>
              Удалить
            </button>
          </li>
        ))}
        {agents.length === 0 && <li className="muted">Агентов нет</li>}
      </ul>
      <form onSubmit={addAgent}>
        <label>
          Relay агента
          <select value={selectedRelay} onChange={(e) => setRelayId(e.target.value)} disabled={relays.length === 0}>
            {relays.map((relay) => (
              <option key={relay.id} value={relay.id}>
                {relay.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          Имя нового агента
          <input value={agentName} onChange={(e) => setAgentName(e.target.value)} placeholder="api" />
        </label>
        <button type="submit" disabled={busy || issued !== null || !agentName.trim() || !selectedRelay}>
          Добавить агента
        </button>
      </form>
    </section>
  );
}
