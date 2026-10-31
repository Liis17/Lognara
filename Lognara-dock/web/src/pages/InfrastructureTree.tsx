import { memo, type RefCallback, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import { age, bytes, corePresentation, plural, REASON, STATUS } from "../infraPresentation";
import {
  groupInfrastructure,
  hasInfrastructureTreeCard,
  infraNodeKey,
  infrastructureTreeEdges,
  infrastructureTreeSelectionText,
  layoutInfrastructureTree,
  selectedInfrastructureTreeKeys,
} from "../infraTree";
import { useInfrastructure } from "../useInfrastructure";
import type { InfraNode, InfrastructureResponse } from "../types";

const clock = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const number = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 1 });

function Chip({ tone, children }: { tone: string; children: string }) {
  return <span className={`node-status ${tone}`}>{children}</span>;
}

const NodeButton = memo(function NodeButton({
  node, asOf, selected, onSelect,
}: { node: InfraNode; asOf: string; selected: boolean; onSelect: (key: string) => void }) {
  const key = infraNodeKey(node);
  const status = STATUS[node.status];
  return <button
    type="button"
    className={`infra-tree-node${selected ? " is-selected" : ""}`}
    data-infra-node-key={key}
    aria-pressed={selected}
    onClick={() => onSelect(key)}
  >
    <span className="infra-tree-node-head"><strong>{node.name}</strong><Chip tone={status.tone}>{status.label}</Chip></span>
    <span className="infra-tree-node-sub">{node.kind} #{node.id} · последнее событие {age(node.last_event_at, asOf)}</span>
    {node.reason && <span className="infra-tree-node-sub">{REASON[node.reason]}</span>}
  </button>;
});

const RelayButton = memo(function RelayButton({
  node, agents, selected, onSelect,
}: { node: InfraNode; agents: InfraNode[]; selected: boolean; onSelect: (key: string) => void }) {
  const key = infraNodeKey(node);
  const status = STATUS[node.status];
  return <button
    type="button"
    className={`infra-tree-node${selected ? " is-selected" : ""}`}
    data-infra-node-key={key}
    aria-pressed={selected}
    onClick={() => onSelect(key)}
  >
    <span className="infra-tree-node-head"><strong>{node.name}</strong><Chip tone={status.tone}>{status.label}</Chip></span>
    <span className="infra-tree-node-sub">relay #{node.id} · {agents.length} {plural(agents.length, ["агент", "агента", "агентов"])}</span>
    <span className="infra-tree-node-sub">{node.events_per_second == null ? "скорость: нет данных" : `${number.format(node.events_per_second)} событий/с`}</span>
  </button>;
});

function InfraTreeGraph({ data, stale }: { data: InfrastructureResponse; stale: boolean }) {
  const groups = useMemo(() => groupInfrastructure(data.nodes), [data.nodes]);
  const [selection, setSelection] = useState<string | null>(null);
  const [measuredHeights, setMeasuredHeights] = useState<ReadonlyMap<string, number>>(() => new Map());
  const scrollRef = useRef<HTMLDivElement>(null);
  const cardElements = useRef(new Map<HTMLDivElement, string>());
  const elementsByKey = useRef(new Map<string, HTMLDivElement>());
  const cardRefs = useRef(new Map<string, RefCallback<HTMLDivElement>>());
  const observer = useRef<ResizeObserver | null>(null);
  const pendingHeights = useRef(new Map<string, number>());
  const frame = useRef<number | null>(null);

  useEffect(() => {
    const resizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const key = cardElements.current.get(entry.target as HTMLDivElement);
        if (key) pendingHeights.current.set(key, Math.ceil(entry.contentRect.height));
      }
      if (frame.current !== null) return;
      frame.current = requestAnimationFrame(() => {
        frame.current = null;
        const updates = [...pendingHeights.current];
        pendingHeights.current.clear();
        setMeasuredHeights((current) => {
          let next: Map<string, number> | null = null;
          for (const [key, height] of updates) {
            if (current.get(key) === height) continue;
            next ??= new Map(current);
            next.set(key, height);
          }
          return next ?? current;
        });
      });
    });
    observer.current = resizeObserver;
    for (const element of cardElements.current.keys()) resizeObserver.observe(element);
    return () => {
      resizeObserver.disconnect();
      observer.current = null;
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      frame.current = null;
      pendingHeights.current.clear();
      cardElements.current.clear();
      elementsByKey.current.clear();
      cardRefs.current.clear();
    };
  }, []);

  const cardRef = (key: string): RefCallback<HTMLDivElement> => {
    let callback = cardRefs.current.get(key);
    if (!callback) {
      callback = (element) => {
        const previous = elementsByKey.current.get(key);
        if (previous) {
          observer.current?.unobserve(previous);
          cardElements.current.delete(previous);
          elementsByKey.current.delete(key);
        }
        if (element) {
          cardElements.current.set(element, key);
          elementsByKey.current.set(key, element);
          observer.current?.observe(element);
        } else {
          cardRefs.current.delete(key);
        }
      };
      cardRefs.current.set(key, callback);
    }
    return callback;
  };

  const layout = useMemo(() => layoutInfrastructureTree(groups, measuredHeights), [groups, measuredHeights]);
  const edges = useMemo(() => infrastructureTreeEdges(layout), [layout]);
  const groupsByKey = useMemo(() => new Map(groups.map((group) => [group.key, group])), [groups]);
  const selectedKeys = useMemo(() => selectedInfrastructureTreeKeys(selection, groups), [selection, groups]);
  const selectionText = useMemo(() => infrastructureTreeSelectionText(selection, groups), [selection, groups]);
  const visibleKeys = useMemo(() => new Set(layout.positions.map((position) => position.key)), [layout]);

  useEffect(() => {
    setMeasuredHeights((current) => {
      const next = new Map([...current].filter(([key]) => visibleKeys.has(key)));
      return next.size === current.size ? current : next;
    });
    if (selection && !hasInfrastructureTreeCard(selection, groups)) {
      setSelection(null);
      if (document.activeElement === document.body) requestAnimationFrame(() => scrollRef.current?.focus());
    }
  }, [groups, selection, visibleKeys]);

  const select = useCallback((key: string) => setSelection((current) => current === key ? null : key), []);
  const core = data.core;
  const coreState = corePresentation(core.status);

  return <>
    {!data.registry && <p className="error infra-tree-registry" role="alert">Не удалось получить список узлов из Core. Статус Core показывается отдельно. <Link to="/access">Управление подключениями</Link>.</p>}
    {data.registry && data.nodes.length === 0 && <p className="empty infra-tree-registry">Нет зарегистрированных relay и агентов. Создайте их в <Link to="/access">Подключениях</Link>.</p>}
    <section className={`panel infra-tree-panel${stale ? " infra-tree-stale" : ""}`} aria-label="Дерево инфраструктуры">
      <div className="panel-head infra-tree-panel-head"><h2>Граф распределения</h2><span className="muted">Agent → Relay → Core → Dock</span></div>
      <div
        className="infra-tree-scroll"
        ref={scrollRef}
        role="group"
        aria-label="Дерево инфраструктуры"
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape" && selection) {
            event.preventDefault();
            setSelection(null);
          }
        }}
      >
        <div className="infra-tree-canvas" style={{ width: layout.width, height: layout.height }}>
          <div className="infra-tree-selection" aria-live="polite" aria-atomic="true">{selectionText}</div>
          <div className="infra-tree-column" style={{ left: 20 }}>Agent</div>
          <div className="infra-tree-column" style={{ left: 400 }}>Relay</div>
          <div className="infra-tree-column" style={{ left: 680 }}>Core</div>
          <div className="infra-tree-column" style={{ left: 900 }}>Dock</div>
          <svg className="infra-tree-svg" width={layout.width} height={layout.height} viewBox={`0 0 ${layout.width} ${layout.height}`} aria-hidden="true">
            {edges.map((edge) => <path
              key={edge.key}
              className={`infra-tree-edge${selectedKeys.has(edge.from) && selectedKeys.has(edge.to) ? " is-active" : ""}`}
              data-infra-edge-key={edge.key}
              d={edge.path}
              fill="none"
            />)}
          </svg>
          {layout.positions.map((position) => <div
            key={position.key}
            ref={cardRef(position.key)}
            className="infra-tree-card-slot"
            data-infra-card-key={position.key}
            style={{ left: position.x, top: position.y, width: position.width }}
          >
            {position.kind === "agent" && position.node && <NodeButton
              node={position.node}
              asOf={data.as_of}
              selected={selectedKeys.has(position.key)}
              onSelect={select}
            />}
            {position.kind === "relay" && position.node && <RelayButton
              node={position.node}
              agents={groupsByKey.get(position.groupKey ?? "")?.agents ?? []}
              selected={selectedKeys.has(position.key)}
              onSelect={select}
            />}
            {position.kind === "orphan" && <div className="infra-tree-orphan" role="note">
              <strong>{position.groupKey === "orphan-unknown" ? "ID relay неизвестен" : `Relay #${(position.groupKey ?? "").slice("orphan-".length)} отсутствует в реестре`}</strong>
              <span>Путь через Core не показан</span>
            </div>}
            {position.kind === "core" && <button
              type="button"
              className={`infra-tree-node infra-tree-system${selectedKeys.has("core") ? " is-selected" : ""}`}
              data-infra-node-key="core"
              aria-pressed={selectedKeys.has("core")}
              onClick={() => select("core")}
            >
              <span className="infra-tree-node-head"><strong>Core</strong><Chip tone={coreState.tone}>{coreState.text}</Chip></span>
              <span className="infra-tree-node-sub">{core.version ? `v${core.version}` : "версия неизвестна"}{core.visibility_lag_ms == null ? "" : ` · lag ${number.format(core.visibility_lag_ms / 1000)} с`}</span>
              <span className="infra-tree-node-sub">WAL {core.wal_bytes == null ? "—" : bytes(core.wal_bytes)}</span>
            </button>}
            {position.kind === "dock" && <button
              type="button"
              className={`infra-tree-node infra-tree-system${selectedKeys.has("dock") ? " is-selected" : ""}`}
              data-infra-node-key="dock"
              aria-pressed={selectedKeys.has("dock")}
              onClick={() => select("dock")}
            >
              <span className="infra-tree-node-head"><strong>Dock</strong><Chip tone={stale ? "mute" : "ok"}>{stale ? "Не подтверждено" : "Онлайн"}</Chip></span>
              <span className="infra-tree-node-sub">v{data.dock.version} · 1 инстанс</span>
              <span className="infra-tree-node-sub">SSE {data.dock.sse_clients} · сессий {data.dock.sessions ?? "нет данных"}</span>
            </button>}
          </div>)}
        </div>
      </div>
      <p className="page-note infra-tree-note">Связи показывают соответствие реестру; они не подтверждают текущую доступность сети. Выберите карточку мышью или клавиатурой; повторный выбор и Escape сбрасывают выделение.</p>
    </section>
  </>;
}

export default function InfrastructureTree({ onUnauthorized }: { onUnauthorized: () => void }) {
  const { data, loadedAt, error, loading, refresh } = useInfrastructure(onUnauthorized);

  return <section className="infra-tree-page page-stack">
    <header className="summary infra-tree-summary">
      <div>
        <div className="infra-tree-title"><Link className="button" to="/infrastructure">← Инфраструктура</Link><h1>Дерево инфраструктуры</h1></div>
        <p className="muted">Agent → Relay → Core → Dock</p>
      </div>
      <button type="button" className="button" disabled={loading} onClick={refresh}>
        {loadedAt ? `Обновлено ${clock.format(loadedAt)}` : "Обновить"}
      </button>
    </header>
    {error && <p className="error" role="alert">Не удалось обновить состояние: {error}. {loadedAt ? `Последний успешный снимок ${clock.format(loadedAt)}; данные устарели.` : "Снимок ещё не получен."}</p>}
    {!data && loading && <p className="empty" role="status">Загружаем дерево инфраструктуры…</p>}
    {!data && !loading && <p className="empty" role="status">Данные инфраструктуры пока недоступны. Попробуйте обновить страницу состояния.</p>}
    {data && <InfraTreeGraph data={data} stale={Boolean(error)} />}
  </section>;
}
