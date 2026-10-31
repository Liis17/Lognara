import type { InfraNode } from "./types.ts";

export interface InfraTreeGroup {
  key: string;
  relayId: number | null;
  relay: InfraNode | null;
  agents: InfraNode[];
  orphan: boolean;
}

export interface InfraTreePosition {
  key: string;
  kind: "agent" | "relay" | "core" | "dock" | "orphan";
  x: number;
  y: number;
  width: number;
  height: number;
  node?: InfraNode;
  groupKey?: string;
}

export interface InfraTreeLayout {
  width: number;
  height: number;
  positions: InfraTreePosition[];
  groups: InfraTreeGroup[];
}

export interface InfraTreeEdge {
  key: string;
  from: string;
  to: string;
  path: string;
}

export function infraNodeKey(node: Pick<InfraNode, "kind" | "id">): string {
  return `${node.kind}-${node.id}`;
}

function compareRelayId(a: number | null, b: number | null): number {
  if (a === null) return b === null ? 0 : 1;
  if (b === null) return -1;
  return a - b;
}

export function groupInfrastructure(nodes: InfraNode[]): InfraTreeGroup[] {
  const relays = new Map<number, InfraNode>();
  const agentsByRelay = new Map<number | null, InfraNode[]>();
  for (const node of nodes) {
    if (node.kind === "relay") relays.set(node.id, node);
    else {
      const agents = agentsByRelay.get(node.relay_id) ?? [];
      agents.push(node);
      agentsByRelay.set(node.relay_id, agents);
    }
  }

  const groups: InfraTreeGroup[] = [...relays.values()].map((relay) => ({
    key: `relay-${relay.id}`,
    relayId: relay.id,
    relay,
    agents: (agentsByRelay.get(relay.id) ?? []).sort((a, b) => a.id - b.id),
    orphan: false,
  }));

  for (const [relayId, agents] of agentsByRelay) {
    if (relayId !== null && relays.has(relayId)) continue;
    const key = relayId === null ? "orphan-unknown" : `orphan-${relayId}`;
    groups.push({ key, relayId, relay: null, agents: agents.sort((a, b) => a.id - b.id), orphan: true });
  }
  return groups.sort((a, b) => compareRelayId(a.relayId, b.relayId));
}

const TREE_WIDTH = 1100;
export const TREE_START_Y = 112;
const GROUP_GAP = 24;
const GROUP_PADDING = 12;
const AGENT_GAP = 14;
const AGENT_X = 20;
const AGENT_WIDTH = 280;
const RELAY_X = 400;
const RELAY_WIDTH = 230;
const CORE_X = 680;
const CORE_WIDTH = 170;
const DOCK_X = 900;
const DOCK_WIDTH = 180;

export function layoutInfrastructureTree(
  groups: InfraTreeGroup[],
  measuredHeights: ReadonlyMap<string, number> = new Map(),
): InfraTreeLayout {
  const heightOf = (key: string, fallback: number) => measuredHeights.get(key) ?? fallback;
  const positions: InfraTreePosition[] = [];
  let groupTop = TREE_START_Y;

  for (const group of groups) {
    const agentHeights = group.agents.map((agent) => heightOf(infraNodeKey(agent), 58));
    const agentsHeight = agentHeights.reduce((sum, height) => sum + height, 0) + Math.max(0, group.agents.length - 1) * AGENT_GAP;
    const relayKey = group.relay ? infraNodeKey(group.relay) : group.key;
    const relayHeight = heightOf(relayKey, group.orphan ? 66 : 92);
    const contentHeight = Math.max(agentsHeight, relayHeight);
    const groupHeight = contentHeight + GROUP_PADDING * 2;
    let agentTop = groupTop + GROUP_PADDING + (contentHeight - agentsHeight) / 2;

    group.agents.forEach((agent, index) => {
      const height = agentHeights[index];
      positions.push({
        key: infraNodeKey(agent), kind: "agent", x: AGENT_X, y: agentTop,
        width: AGENT_WIDTH, height, node: agent, groupKey: group.key,
      });
      agentTop += height + AGENT_GAP;
    });

    const height = relayHeight;
    positions.push({
      key: group.relay ? infraNodeKey(group.relay) : group.key,
      kind: group.orphan ? "orphan" : "relay",
      x: RELAY_X,
      y: groupTop + (groupHeight - height) / 2,
      width: RELAY_WIDTH,
      height,
      node: group.relay ?? undefined,
      groupKey: group.key,
    });
    groupTop += groupHeight + GROUP_GAP;
  }

  const groupsBottom = groups.length ? groupTop - GROUP_GAP : TREE_START_Y + 92;
  const centerY = groups.length ? (TREE_START_Y + groupsBottom) / 2 : TREE_START_Y + 46;
  const coreHeight = heightOf("core", 112);
  const dockHeight = heightOf("dock", 112);
  positions.push({ key: "core", kind: "core", x: CORE_X, y: centerY - coreHeight / 2, width: CORE_WIDTH, height: coreHeight });
  positions.push({ key: "dock", kind: "dock", x: DOCK_X, y: centerY - dockHeight / 2, width: DOCK_WIDTH, height: dockHeight });

  return {
    width: TREE_WIDTH,
    height: Math.max(groupsBottom + 32, centerY + Math.max(coreHeight, dockHeight) / 2 + 32, 190),
    positions,
    groups,
  };
}

function centerY(position: InfraTreePosition): number {
  return position.y + position.height / 2;
}

function curve(from: InfraTreePosition, to: InfraTreePosition): string {
  const startX = from.x + from.width;
  const startY = centerY(from);
  const endX = to.x;
  const endY = centerY(to);
  const controlX = (startX + endX) / 2;
  return `M ${startX} ${startY} C ${controlX} ${startY}, ${controlX} ${endY}, ${endX} ${endY}`;
}

export function infrastructureTreeEdges(layout: InfraTreeLayout): InfraTreeEdge[] {
  const positions = new Map(layout.positions.map((position) => [position.key, position]));
  const edges: InfraTreeEdge[] = [];
  for (const group of layout.groups) {
    if (!group.relay || group.orphan) continue;
    const relayKey = infraNodeKey(group.relay);
    const relay = positions.get(relayKey)!;
    for (const agent of group.agents) {
      const agentKey = infraNodeKey(agent);
      edges.push({ key: `${agentKey}:${relayKey}`, from: agentKey, to: relayKey, path: curve(positions.get(agentKey)!, relay) });
    }
    edges.push({ key: `${relayKey}:core`, from: relayKey, to: "core", path: curve(relay, positions.get("core")!) });
  }
  const core = positions.get("core")!;
  const dock = positions.get("dock")!;
  edges.push({
    key: "core:dock", from: "core", to: "dock",
    path: `M ${core.x + core.width} ${centerY(core)} L ${dock.x} ${centerY(dock)}`,
  });
  return edges;
}

export function hasInfrastructureTreeCard(key: string, groups: InfraTreeGroup[]): boolean {
  if (key === "core" || key === "dock") return true;
  return groups.some((group) => group.relay && infraNodeKey(group.relay) === key || group.agents.some((agent) => infraNodeKey(agent) === key));
}

export function selectedInfrastructureTreeKeys(selection: string | null, groups: InfraTreeGroup[]): Set<string> {
  if (!selection) return new Set();
  if (selection === "core" || selection === "dock") {
    const connected = new Set(["core", "dock"]);
    for (const group of groups) {
      if (group.orphan || !group.relay) continue;
      connected.add(infraNodeKey(group.relay));
      for (const agent of group.agents) connected.add(infraNodeKey(agent));
    }
    return connected;
  }

  for (const group of groups) {
    if (group.relay && infraNodeKey(group.relay) === selection) {
      const connected = new Set([selection, "core", "dock"]);
      for (const agent of group.agents) connected.add(infraNodeKey(agent));
      return connected;
    }
    const agent = group.agents.find((candidate) => infraNodeKey(candidate) === selection);
    if (agent) {
      if (group.orphan || !group.relay) return new Set([selection]);
      return new Set([selection, infraNodeKey(group.relay), "core", "dock"]);
    }
  }
  return new Set();
}

function agentCount(count: number): string {
  const tens = count % 100, ones = count % 10;
  const noun = tens >= 11 && tens <= 14 ? "агентов" : ones === 1 ? "агент" : ones >= 2 && ones <= 4 ? "агента" : "агентов";
  return `${count} ${noun}`;
}

function relayGroupCount(count: number): string {
  const tens = count % 100, ones = count % 10;
  const noun = tens >= 11 && tens <= 14 ? "relay-групп" : ones === 1 ? "relay-группа" : ones >= 2 && ones <= 4 ? "relay-группы" : "relay-групп";
  return `${count} ${noun}`;
}

function connectedGroupSummary(groups: InfraTreeGroup[]): string {
  const connected = groups.filter((group) => !group.orphan && group.relay !== null);
  const connectedAgents = connected.reduce((count, group) => count + group.agents.length, 0);
  return `${relayGroupCount(connected.length)}, ${agentCount(connectedAgents)} в них`;
}

export function infrastructureTreeSelectionText(selection: string | null, groups: InfraTreeGroup[]): string {
  if (!selection) return "Выберите карточку, чтобы увидеть её группу и путь.";
  for (const group of groups) {
    if (group.relay && infraNodeKey(group.relay) === selection) {
      return `Relay ${group.relay.name} (#${group.relay.id}), ${agentCount(group.agents.length)} → Core → Dock`;
    }
    const agent = group.agents.find((candidate) => infraNodeKey(candidate) === selection);
    if (agent) {
      if (group.orphan || !group.relay) {
        const missing = group.relayId === null ? "ID relay неизвестен" : `relay #${group.relayId} отсутствует в реестре`;
        return `Агент ${agent.name} (#${agent.id}); ${missing}. Дальнейший путь не показан.`;
      }
      return `Агент ${agent.name} (#${agent.id}) → Relay ${group.relay.name} (#${group.relay.id}) → Core → Dock`;
    }
  }
  if (selection === "core") return `Core → Dock; выше показаны только зарегистрированные связи: ${connectedGroupSummary(groups)}.`;
  if (selection === "dock") return `Все зарегистрированные relay и их агенты: ${connectedGroupSummary(groups)} → Core → Dock.`;
  return "Выберите карточку, чтобы увидеть её группу и путь.";
}
