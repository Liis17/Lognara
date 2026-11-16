import { useId } from "react";
import { api, type Agent, type NodeScope, type Relay } from "../api";
import ServiceCombobox from "./ServiceCombobox";

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
  appearance = "default",
}: {
  draft: AccessDraft;
  onChange: (next: AccessDraft) => void;
  nodes: NodeLists | null;
  appearance?: "default" | "cards";
}) {
  const scopeName = useId();
  const update = (patch: Partial<AccessDraft>) => onChange({ ...draft, ...patch });
  const relays = nodes?.relays ?? [];
  const agents = (nodes?.agents ?? []).filter((agent) => agent.relay_id === draft.relayId);
  const nodesUnavailable = nodes?.failed ?? false;
  const relayOptions = relays.map((relay) => ({ value: String(relay.id), label: relay.name, secondary: `Relay #${relay.id}` }));
  const agentOptions = agents.map((agent) => ({ value: String(agent.id), label: agent.name, secondary: `Агент #${agent.id}` }));
  const permissionOptions = [
    { key: "logs" as const, title: "Логи", description: "Поиск событий, просмотр текста сообщений и трассировок." },
    { key: "metrics" as const, title: "Метрики", description: "Просмотр определений метрик и временных рядов." },
    { key: "alertsWrite" as const, title: "Управление алертами логов и метрик", description: "Логовые алерты требуют чтения логов, а алерты метрик — чтения метрик." },
  ];

  const changePermission = (key: typeof permissionOptions[number]["key"], checked: boolean) => {
    if (key === "logs") update({ logs: checked, alertsWrite: (checked || draft.metrics) && draft.alertsWrite });
    else if (key === "metrics") update({ metrics: checked, alertsWrite: (checked || draft.logs) && draft.alertsWrite });
    else update({ alertsWrite: checked });
  };

  const selectRelay = (value: string | null) => update({ relayId: value === null ? null : Number(value), agentId: null });

  return (
    <>
      <fieldset className={`access-fields${appearance === "cards" ? " access-fields-modern" : ""}`}>
        <legend>Права</legend>
        {permissionOptions.map((permission) => (
          <label className={appearance === "cards" ? "access-permission-card" : undefined} key={permission.key}>
            <input
              type="checkbox"
              aria-label={permission.title}
              checked={draft[permission.key]}
              disabled={permission.key === "alertsWrite" && !draft.logs && !draft.metrics}
              onChange={(event) => changePermission(permission.key, event.target.checked)}
            />
            {appearance === "cards" ? (
              <span>
                <strong>{permission.title}</strong>
                <small>{permission.description}</small>
              </span>
              ) : permission.title}
          </label>
        ))}
        {appearance !== "cards" && <>
          <p className="muted">Логовые алерты доступны при чтении логов, алерты метрик — при чтении метрик.</p>
          {!draft.logs && !draft.metrics && <p className="muted">Выберите доступ к логам или метрикам, чтобы включить управление алертами.</p>}
        </>}
        {appearance === "cards" && <p className="muted">Логовые алерты доступны при чтении логов, алерты метрик — при чтении метрик.</p>}
      </fieldset>
      <fieldset className={`access-fields${appearance === "cards" ? " access-fields-modern" : ""}`}>
        <legend>Доступ к данным</legend>
        {appearance === "cards" ? (
          <div className="access-scope-options">
            <label className="access-scope-card">
              <input type="radio" name={scopeName} aria-label="Все узлы" checked={draft.kind === "all"} onChange={() => update({ kind: "all" })} />
              <span><strong>Все узлы</strong><small>Доступ ко всем relay и агентам.</small></span>
            </label>
            <label className="access-scope-card">
              <input type="radio" name={scopeName} aria-label="Relay и его агенты" checked={draft.kind === "relay"} disabled={nodesUnavailable} onChange={() => update({ kind: "relay" })} />
              <span><strong>Relay и его агенты</strong><small>Ограничить токен одним relay и его агентами.</small></span>
            </label>
            <label className="access-scope-card">
              <input type="radio" name={scopeName} aria-label="Конкретный агент" checked={draft.kind === "agent"} disabled={nodesUnavailable} onChange={() => update({ kind: "agent" })} />
              <span><strong>Конкретный агент</strong><small>Ограничить токен одним агентом.</small></span>
            </label>
          </div>
        ) : <>
          <label>
            <input type="radio" name={scopeName} checked={draft.kind === "all"} onChange={() => update({ kind: "all" })} />
            Все узлы
          </label>
          <label>
            <input type="radio" name={scopeName} checked={draft.kind === "relay"} disabled={nodesUnavailable} onChange={() => update({ kind: "relay" })} />
            Relay и его агенты
          </label>
          <label>
            <input type="radio" name={scopeName} checked={draft.kind === "agent"} disabled={nodesUnavailable} onChange={() => update({ kind: "agent" })} />
            Конкретный агент
          </label>
        </>}
        {draft.kind === "relay" && (appearance === "cards" ? (
          <ServiceCombobox
            label="Relay"
            value={draft.relayId === null ? null : String(draft.relayId)}
            options={relayOptions}
            onChange={selectRelay}
            disabled={nodesUnavailable}
            emptyLabel="Выберите relay"
          />
        ) : (
          <select aria-label="Relay" value={draft.relayId ?? ""} onChange={(event) => update({ relayId: Number(event.target.value) || null, agentId: null })}>
            <option value="">Выберите relay</option>
            {relays.map((relay) => <option value={relay.id} key={relay.id}>{relay.name}</option>)}
          </select>
        ))}
        {draft.kind === "agent" && (
          appearance === "cards" ? (
            <div className="access-nested">
              <ServiceCombobox
                label="Relay"
                value={draft.relayId === null ? null : String(draft.relayId)}
                options={relayOptions}
                onChange={selectRelay}
                disabled={nodesUnavailable}
                emptyLabel="Выберите relay"
              />
              <ServiceCombobox
                label="Агент"
                value={draft.agentId === null ? null : String(draft.agentId)}
                options={agentOptions}
                onChange={(value) => update({ agentId: value === null ? null : Number(value) })}
                disabled={nodesUnavailable || draft.relayId === null}
                emptyLabel="Выберите агента"
              />
            </div>
          ) : (
            <div className="access-nested">
              <select aria-label="Relay" value={draft.relayId ?? ""} onChange={(event) => update({ relayId: Number(event.target.value) || null, agentId: null })}>
                <option value="">Выберите relay</option>
                {relays.map((relay) => <option value={relay.id} key={relay.id}>{relay.name}</option>)}
              </select>
              <select aria-label="Агент" value={draft.agentId ?? ""} disabled={draft.relayId === null} onChange={(event) => update({ agentId: Number(event.target.value) || null })}>
                <option value="">Выберите агента</option>
                {agents.map((agent) => <option value={agent.id} key={agent.id}>{agent.name}</option>)}
              </select>
            </div>
          )
        )}
        {nodesUnavailable && <p className="muted">Список узлов недоступен, доступен только вариант «Все узлы».</p>}
      </fieldset>
    </>
  );
}
