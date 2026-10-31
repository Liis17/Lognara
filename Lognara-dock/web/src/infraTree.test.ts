import { test } from "node:test";
import assert from "node:assert/strict";
import {
  groupInfrastructure,
  infraNodeKey,
  infrastructureTreeEdges,
  infrastructureTreeSelectionText,
  layoutInfrastructureTree,
  selectedInfrastructureTreeKeys,
  TREE_START_Y,
} from "./infraTree.ts";
import type { InfraNode } from "./types.ts";

function node(kind: "relay" | "agent", id: number, name: string, relay_id: number | null = null): InfraNode {
  return {
    kind, id, name, relay_id, relay: kind === "agent" && relay_id !== null ? `relay-${relay_id}` : null,
    status: "online", reason: null, heartbeat_at: null, last_event_at: null, version: null,
    spool: null, delivery_failures: null, events_per_second: null, source: null,
  };
}

test("groups relay and agent rows by ID in stable order and retains empty relays", () => {
  const groups = groupInfrastructure([
    node("agent", 8, "same-name", 2), node("relay", 2, "same-name"),
    node("agent", 3, "first", 2), node("relay", 1, "empty"),
  ]);
  assert.deepEqual(groups.map((group) => group.relayId), [1, 2]);
  assert.deepEqual(groups[0].agents, []);
  assert.deepEqual(groups[1].agents.map((agent) => agent.id), [3, 8]);
  assert.equal(groups[1].agents[0].name, "first");
});

test("missing relay IDs form labelled orphan groups with no fabricated edges", () => {
  const groups = groupInfrastructure([
    node("agent", 9, "orphan", 99), node("agent", 10, "unknown", null),
  ]);
  assert.deepEqual(groups.map((group) => [group.relayId, group.orphan]), [[99, true], [null, true]]);
  assert.equal(groups[0].relay, null);
  assert.equal(groups[0].agents[0].relay, "relay-99");
  const layout = layoutInfrastructureTree(groups);
  assert.deepEqual(infrastructureTreeEdges(layout).map((edge) => edge.key), ["core:dock"]);
  assert.match(infrastructureTreeSelectionText("agent-9", groups), /relay #99 отсутствует в реестре/);
  assert.deepEqual([...selectedInfrastructureTreeKeys("agent-9", groups)], ["agent-9"]);
});

test("relay and core cards use group and overall centers with measured card heights", () => {
  const groups = groupInfrastructure([
    node("agent", 2, "agent-b", 1), node("relay", 1, "relay"), node("agent", 1, "agent-a", 1),
    node("relay", 3, "empty-relay"),
  ]);
  const heights = new Map([["agent-1", 90], ["agent-2", 70], ["relay-1", 100], ["relay-3", 86], ["core", 150], ["dock", 120]]);
  const layout = layoutInfrastructureTree(groups, heights);
  const position = (key: string) => layout.positions.find((item) => item.key === key)!;
  const firstAgent = position("agent-1");
  const secondAgent = position("agent-2");
  const firstRelay = position("relay-1");
  assert.equal(firstAgent.height, 90);
  assert.equal(secondAgent.height, 70);
  assert.equal(firstRelay.y + firstRelay.height / 2, (firstAgent.y + secondAgent.y + secondAgent.height) / 2);
  const emptyRelay = position("relay-3");
  assert.equal(emptyRelay.y + emptyRelay.height / 2, TREE_START_Y + 198 + 24 + (110 / 2));
  const lastGroupBottom = emptyRelay.y + emptyRelay.height + 12;
  const center = (TREE_START_Y + lastGroupBottom) / 2;
  assert.equal(position("core").y + position("core").height / 2, center);
  assert.equal(position("dock").y + position("dock").height / 2, center);
});

test("selection follows IDs through agents, relay descendants, core, and dock", () => {
  const groups = groupInfrastructure([
    node("relay", 7, "duplicate"), node("agent", 7, "duplicate", 7),
    node("relay", 8, "second"), node("agent", 2, "second-agent", 8),
    node("agent", 99, "orphan", 99),
  ]);
  assert.notEqual(infraNodeKey(groups[0].relay!), infraNodeKey(groups[0].agents[0]));
  assert.deepEqual([...selectedInfrastructureTreeKeys("agent-7", groups)].sort(), ["agent-7", "core", "dock", "relay-7"]);
  assert.deepEqual([...selectedInfrastructureTreeKeys("relay-7", groups)].sort(), ["agent-7", "core", "dock", "relay-7"]);
  assert.deepEqual([...selectedInfrastructureTreeKeys("core", groups)].sort(), ["agent-2", "agent-7", "core", "dock", "relay-7", "relay-8"]);
  assert.deepEqual([...selectedInfrastructureTreeKeys("dock", groups)], [...selectedInfrastructureTreeKeys("core", groups)]);
  assert.match(infrastructureTreeSelectionText("relay-7", groups), /Relay duplicate \(#7\), 1 агент/);
});

test("edge endpoints align with measured card edges and centers", () => {
  const groups = groupInfrastructure([node("relay", 5, "relay"), node("agent", 4, "agent", 5)]);
  const layout = layoutInfrastructureTree(groups, new Map([["agent-4", 88], ["relay-5", 104]]));
  const positions = new Map(layout.positions.map((position) => [position.key, position]));
  const edge = infrastructureTreeEdges(layout).find((item) => item.key === "agent-4:relay-5")!;
  const values = edge.path.match(/^M ([\d.]+) ([\d.]+) C [\d.]+ [\d.]+, [\d.]+ [\d.]+, ([\d.]+) ([\d.]+)$/)!;
  const agent = positions.get("agent-4")!;
  const relay = positions.get("relay-5")!;
  assert.deepEqual(values.slice(1).map(Number), [agent.x + agent.width, agent.y + agent.height / 2, relay.x, relay.y + relay.height / 2]);
});
