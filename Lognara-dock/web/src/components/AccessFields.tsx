import { useId } from "react";
import { api, type Agent, type NodeScope, type Relay } from "../api";

/** Черновик выбора прав и скоупа для токена или OAuth-согласия. */
export interface AccessDraft {
  logs: boolean;
  metrics: boolean;
  alertsWrite: boolean;
  kind: NodeScope["kind"];
  relayId: number | null;
  agentId: number | null;
}

export const defaultAccessDraft: AccessDraft = {
  logs: true,
  metrics: true,
  alertsWrite: false,
  kind: "all",
  relayId: null,
  agentId: null,
};

/** Тело прав для API или null, если нет доступа к данным либо узел не выбран. */
export function accessBody(draft: AccessDraft) {
  let nodeScope: NodeScope | null = { kind: "all" };
  if (draft.kind === "relay") nodeScope = draft.relayId === null ? null : { kind: "relay", id: draft.relayId };
  if (draft.kind === "agent") nodeScope = draft.agentId === null ? null : { kind: "agent", id: draft.agentId };
  if (!nodeScope || !(draft.logs || draft.metrics)) return null;
  return {
    logs: draft.logs,
    metrics: draft.metrics,
    alerts_write: (draft.logs || draft.metrics) && draft.alertsWrite,
    node_scope: nodeScope,
  };
}

export interface NodeLists {
  relays: Relay[];
  agents: Agent[];
  /** Core не ответил: доступен только вариант «Все узлы». */
  failed: boolean;
}

export async function loadNodeLists(): Promise<NodeLists> {
  try {
    const [relays, agents] = await Promise.all([
      api<{ relays: Relay[] }>("GET", "/api/relays"),
      api<{ agents: Agent[] }>("GET", "/api/agents"),
    ]);
    return { relays: relays.relays, agents: agents.agents, failed: false };
  } catch {
    return { relays: [], agents: [], failed: true };
  }
}

export function AccessFields({
  draft,
  onChange,
  nodes,
}: {
  draft: AccessDraft;
  onChange: (next: AccessDraft) => void;
  nodes: NodeLists | null;
}) {
  const scopeName = useId();
  const update = (patch: Partial<AccessDraft>) => onChange({ ...draft, ...patch });
  const relays = nodes?.relays ?? [];
  const agents = (nodes?.agents ?? []).filter((agent) => agent.relay_id === draft.relayId);
  const nodesUnavailable = nodes?.failed ?? false;
  return (
    <>
      <fieldset className="access-fields">
        <legend>Права</legend>
        <label>
          <input
            type="checkbox"
            checked={draft.logs}
            onChange={(event) => {
              const logs = event.target.checked;
              update({ logs, alertsWrite: (logs || draft.metrics) && draft.alertsWrite });
            }}
          />
          Логи
        </label>
        <label>
          <input
            type="checkbox"
            checked={draft.metrics}
            onChange={(event) => {
              const metrics = event.target.checked;
              update({ metrics, alertsWrite: (metrics || draft.logs) && draft.alertsWrite });
            }}
          />
          Метрики
        </label>
        <label>
          <input
            type="checkbox"
            checked={draft.alertsWrite}
            disabled={!draft.logs && !draft.metrics}
            onChange={(event) => update({ alertsWrite: event.target.checked })}
          />
          Управление алертами логов и метрик
        </label>
        <p className="muted">Логовые алерты доступны при чтении логов, алерты метрик — при чтении метрик.</p>
        {!draft.logs && !draft.metrics && <p className="muted">Выберите доступ к логам или метрикам, чтобы включить управление алертами.</p>}
      </fieldset>
      <fieldset className="access-fields">
        <legend>Доступ к данным</legend>
        <label>
          <input type="radio" name={scopeName} checked={draft.kind === "all"} onChange={() => update({ kind: "all" })} />
          Все узлы
        </label>
        <label>
          <input
            type="radio"
            name={scopeName}
            checked={draft.kind === "relay"}
            disabled={nodesUnavailable}
            onChange={() => update({ kind: "relay" })}
          />
          Relay и его агенты
        </label>
        {draft.kind === "relay" && (
          <select
            aria-label="Relay"
            value={draft.relayId ?? ""}
            onChange={(event) => update({ relayId: Number(event.target.value) || null, agentId: null })}
          >
            <option value="">Выберите relay</option>
            {relays.map((relay) => <option value={relay.id} key={relay.id}>{relay.name}</option>)}
          </select>
        )}
        <label>
          <input
            type="radio"
            name={scopeName}
            checked={draft.kind === "agent"}
            disabled={nodesUnavailable}
            onChange={() => update({ kind: "agent" })}
          />
          Конкретный агент
        </label>
        {draft.kind === "agent" && (
          <div className="access-nested">
            <select
              aria-label="Relay"
              value={draft.relayId ?? ""}
              onChange={(event) => update({ relayId: Number(event.target.value) || null, agentId: null })}
            >
              <option value="">Выберите relay</option>
              {relays.map((relay) => <option value={relay.id} key={relay.id}>{relay.name}</option>)}
            </select>
            <select
              aria-label="Агент"
              value={draft.agentId ?? ""}
              disabled={draft.relayId === null}
              onChange={(event) => update({ agentId: Number(event.target.value) || null })}
            >
              <option value="">Выберите агента</option>
              {agents.map((agent) => <option value={agent.id} key={agent.id}>{agent.name}</option>)}
            </select>
          </div>
        )}
        {nodesUnavailable && <p className="muted">Список узлов недоступен, доступен только вариант «Все узлы».</p>}
      </fieldset>
    </>
  );
}
