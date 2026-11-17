import { type FormEvent, type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, type Agent, errorText, type Relay } from "../api";
import HostIcon from "../components/HostIcon";

type NodeKind = "relay" | "agent" | "host";
type AddTarget = { kind: "relay" } | { kind: "host" } | { kind: "agent"; relayId: number };
type Editing = { kind: NodeKind; id: number; name: string };

/** Токен, который core показывает один раз. */
interface Issued {
  kind: NodeKind;
  name: string;
  token: string;
  envLine: string;
  rotated?: boolean;
}

function kindName(kind: NodeKind) {
  return kind === "relay" ? "relay" : kind === "host" ? "hwagent" : "агент";
}

function nodePath(kind: NodeKind, id: number) {
  const resource = kind === "host" ? "hosts" : `${kind}s`;
  return `/api/${resource}/${id}`;
}

function statusFor(receivedAt: string | undefined, now: number) {
  if (!receivedAt || !Number.isFinite(Date.parse(receivedAt))) return { text: "Нет heartbeat", tone: "mute" };
  return now - Date.parse(receivedAt) <= 60_000
    ? { text: "Онлайн", tone: "ok" }
    : { text: "Отключён", tone: "off" };
}

function NodeStatus({ receivedAt, now }: { receivedAt: string | undefined; now: number }) {
  const status = statusFor(receivedAt, now);
  return <span className={`node-status ${status.tone}`}>{status.text}</span>;
}

function AddSlot({
  label, active, name, disabled, onName, onOpen, onSubmit, onCancel,
}: {
  label: string;
  active: boolean;
  name: string;
  disabled: boolean;
  onName: (value: string) => void;
  onOpen: () => void;
  onSubmit: (event: FormEvent) => void;
  onCancel: () => void;
}) {
  if (!active) return <button type="button" className="access-add-slot" disabled={disabled} onClick={onOpen}>
    <span className="access-add-mark" aria-hidden="true">+</span><span>Добавить {label}</span>
  </button>;
  return <form className="access-add-form" aria-label={`Создать ${label}`} onSubmit={onSubmit}>
    <label>Имя {label}<input autoFocus maxLength={64} value={name} onChange={(event) => onName(event.target.value)} /></label>
    <div className="access-actions">
      <button type="submit" disabled={disabled || !name.trim()}>Создать</button>
      <button type="button" className="quiet" disabled={disabled} onClick={onCancel}>Отмена</button>
    </div>
  </form>;
}

function TokenDialog({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current!;
    dialog.showModal();
    return () => dialog.close();
  }, []);
  // Esc не закрывает: токен показывается один раз, закрытие только кнопкой «Я сохранил токен».
  return <dialog ref={ref} className="token token-dialog" aria-label="Выданный токен" onCancel={(event) => event.preventDefault()}>
    {children}
  </dialog>;
}

function NameEditor({
  value, disabled, onChange, onSubmit, onCancel,
}: {
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
  onSubmit: (event: FormEvent) => void;
  onCancel: () => void;
}) {
  return <form className="access-name-edit" onSubmit={onSubmit}>
    <input aria-label="Новое имя" autoFocus maxLength={64} value={value} onChange={(event) => onChange(event.target.value)} />
    <button type="submit" disabled={disabled || !value.trim()}>Сохранить</button>
    <button type="button" className="quiet" disabled={disabled} onClick={onCancel}>Отмена</button>
  </form>;
}

export default function Access({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [relays, setRelays] = useState<Relay[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [adding, setAdding] = useState<AddTarget | null>(null);
  const [nameDraft, setNameDraft] = useState("");
  const [editing, setEditing] = useState<Editing | null>(null);
  const [issued, setIssued] = useState<Issued | null>(null);
  const [copyStatus, setCopyStatus] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const busyRef = useRef(false);
  const unauthorized = useRef(onUnauthorized);
  unauthorized.current = onUnauthorized;

  const refresh = useCallback(async (duringAction = false) => {
    if (busyRef.current && !duringAction) return;
    try {
      const [relayReply, agentReply] = await Promise.all([
        api<{ relays: Relay[] }>("GET", "/api/relays"),
        api<{ agents: Agent[] }>("GET", "/api/agents"),
      ]);
      setRelays(relayReply.relays);
      setAgents(agentReply.agents);
      setError("");
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 401) unauthorized.current();
      else setError(errorText(cause));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const poll = window.setInterval(() => { void refresh(); }, 10_000);
    const clock = window.setInterval(() => setNow(Date.now()), 10_000);
    return () => { window.clearInterval(poll); window.clearInterval(clock); };
  }, [refresh]);

  const run = async (action: () => Promise<void>) => {
    busyRef.current = true;
    setBusy(true);
    setError("");
    try {
      await action();
      await refresh(true);
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 401) unauthorized.current();
      else if (cause instanceof ApiError && cause.code === "core_upgrade_required") setError("Для переименования обновите Core");
      else setError(errorText(cause));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  const agentsByRelay = new Map<number, Agent[]>();
  for (const agent of agents) {
    const group = agentsByRelay.get(agent.relay_id) ?? [];
    group.push(agent);
    agentsByRelay.set(agent.relay_id, group);
  }
  const hosts = relays.filter((relay) => relay.kind === "host");
  const plainRelays = relays.filter((relay) => relay.kind !== "host");

  const openAdd = (target: AddTarget) => {
    setAdding(target);
    setNameDraft("");
  };
  const isAdding = (target: AddTarget) => adding?.kind === target.kind &&
    (target.kind !== "agent" || (adding.kind === "agent" && adding.relayId === target.relayId));

  const submitAdd = (event: FormEvent) => {
    event.preventDefault();
    if (!adding) return;
    const target = adding;
    const name = nameDraft.trim();
    if (!name) return;
    run(async () => {
      if (target.kind === "relay") {
        const reply = await api<{ relay: Relay; token: string }>("POST", "/api/relays", { name });
        setIssued({ kind: "relay", name: reply.relay.name, token: reply.token, envLine: `LOGNARA_RELAY_CORE_TOKEN=${reply.token}` });
      } else if (target.kind === "host") {
        const reply = await api<{ relay: Relay; token: string }>("POST", "/api/hosts", { name });
        setIssued({ kind: "host", name: reply.relay.name, token: reply.token, envLine: `LOGNARA_HOST_TOKEN=${reply.token}` });
      } else {
        const reply = await api<{ agent: Agent; token: string }>("POST", "/api/agents", { name, relay_id: target.relayId });
        setIssued({ kind: "agent", name: reply.agent.name, token: reply.token, envLine: `LOGNARA_AGENT_TOKEN=${reply.token}` });
      }
      setAdding(null);
      setNameDraft("");
      setCopyStatus("");
    });
  };

  const saveRename = (event: FormEvent) => {
    event.preventDefault();
    if (!editing || !editing.name.trim()) return;
    const target = editing;
    run(async () => {
      await api("PATCH", nodePath(target.kind, target.id), { name: target.name.trim() });
      setEditing(null);
    });
  };

  const rotate = (kind: "agent" | "host", id: number, name: string) => {
    const message = kind === "host"
      ? `Сменить токен hwagent «${name}»? Старый токен перестанет работать сразу. История и привязки алертов сохранятся.`
      : `Перевыпустить токен агента «${name}»? Старый токен перестанет работать после синхронизации relay. Новый токен нужно установить в конфигурации агента.`;
    if (!window.confirm(message)) return;
    run(async () => {
      const reply = await api<{ relay?: Relay; agent?: Agent; token: string }>("POST", `/api/${kind === "host" ? "hosts" : "agents"}/${id}/rotate-token`);
      setIssued({
        kind,
        name: reply.relay?.name ?? reply.agent?.name ?? name,
        token: reply.token,
        envLine: `LOGNARA_${kind === "host" ? "HOST" : "AGENT"}_TOKEN=${reply.token}`,
        rotated: true,
      });
      setCopyStatus("");
    });
  };

  const remove = (kind: NodeKind, id: number, name: string) => {
    if (!window.confirm(`Удалить ${kindName(kind)} «${name}»? Его токен перестанет работать.`)) return;
    run(async () => { await api("DELETE", nodePath(kind, id)); });
  };

  const beginEdit = (kind: NodeKind, id: number, name: string) => setEditing({ kind, id, name });
  const isEditing = (kind: NodeKind, id: number) => editing?.kind === kind && editing.id === id;
  const updateEditName = (name: string) => setEditing((current) => current ? { ...current, name } : current);

  const copy = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopyStatus(`${label} скопирован${label === "Токен" ? "" : "а"}.`);
    } catch {
      setCopyStatus("Не удалось скопировать. Выделите нужный текст в поле и скопируйте вручную.");
    }
  };

  return <section className="access page-stack">
    <header className="access-heading">
      <div><h2>Подключения</h2><p className="muted">Структура узлов и управление именами, доступом и токенами.</p></div>
      <button type="button" className="button" disabled={busy} onClick={() => refresh(true)}>Обновить</button>
    </header>
    {error && <p className="error" role="alert">{error}</p>}

    {issued && <TokenDialog>
      <p><strong>{issued.rotated
        ? `Токен ${issued.kind === "host" ? "hwagent" : issued.kind === "agent" ? "агента" : "relay"} «${issued.name}» перевыпущен.`
        : `${issued.kind === "relay" ? "Relay" : issued.kind === "host" ? "hwagent" : "Агент"} «${issued.name}» создан.`}</strong>{" "}
        Токен показан только сейчас и больше не покажется: {issued.kind === "host" ? "добавьте строку в /etc/lognara/hwagent.env (права 0600)" : "добавьте строку в .env"} и сохраните её.</p>
      {issued.rotated && issued.kind === "host" && <p className="muted">Замените токен в hwagent.env и перезапустите lognara-hwagent. Старый токен уже не принимается.</p>}
      {issued.rotated && issued.kind === "agent" && <p className="muted">Обновите конфигурацию агента. Замена применяется после успешной синхронизации relay, обычно в пределах 10 секунд.</p>}
      <label>Строка для .env<textarea readOnly value={issued.envLine} rows={3} spellCheck={false} /></label>
      <div className="access-actions">
        <button type="button" onClick={() => copy(issued.envLine, "Строка .env")}>Скопировать строку .env</button>
        <button type="button" onClick={() => copy(issued.token, "Токен")}>Скопировать токен</button>
        <button type="button" onClick={() => { setIssued(null); setCopyStatus(""); }}>Я сохранил токен</button>
      </div>
      <p className="access-copy-status" role="status">{copyStatus}</p>
    </TokenDialog>}

    <section className="access-tree" aria-label="Дерево подключений">
      <header className="access-dock-node">
        <span className="access-dock-mark" aria-hidden="true">D</span>
        <div><h3>Dock</h3><p className="muted">Управление подключениями</p></div>
        <span className="node-status ok">Онлайн</span>
      </header>
      <div className="access-tree-branches">
        {plainRelays.map((relay) => {
          const relayAgents = agentsByRelay.get(relay.id) ?? [];
          const relayEditing = isEditing("relay", relay.id);
          return <section className="access-relay-node" key={relay.id} aria-label={`Relay ${relay.name}`}>
            <header className="access-node-header">
              <div className="access-node-identity">
                {relayEditing ? <NameEditor value={editing?.name ?? ""} disabled={busy} onChange={updateEditName} onSubmit={saveRename} onCancel={() => setEditing(null)} /> : <>
                  <strong>Relay · {relay.name}</strong><NodeStatus receivedAt={relay.heartbeat?.received_at} now={now} />
                  <span className="muted">Создан {new Date(relay.created_at).toLocaleString("ru-RU")} · {relayAgents.length} {relayAgents.length === 1 ? "агент" : "агентов"}</span>
                </>}
              </div>
              {!relayEditing && <div className="access-actions">
                <button type="button" className="quiet" disabled={busy} onClick={() => beginEdit("relay", relay.id, relay.name)}>Переименовать</button>
                <button type="button" className="quiet" disabled={busy} onClick={() => remove("relay", relay.id, relay.name)}>Удалить relay</button>
              </div>}
            </header>
            <div className="access-agent-branches" aria-label={`Агенты relay ${relay.name}`}>
              {relayAgents.map((agent) => {
                const agentEditing = isEditing("agent", agent.id);
                return <article className="access-agent-node" key={agent.id} aria-label={`Агент ${agent.name}`}>
                  <div className="access-node-identity">
                    {agentEditing ? <NameEditor value={editing?.name ?? ""} disabled={busy} onChange={updateEditName} onSubmit={saveRename} onCancel={() => setEditing(null)} /> : <>
                      <strong>Agent · {agent.name}</strong><NodeStatus receivedAt={agent.heartbeat?.received_at} now={now} />
                      <span className="muted">Создан {new Date(agent.created_at).toLocaleString("ru-RU")}</span>
                    </>}
                  </div>
                  {!agentEditing && <div className="access-actions">
                    <button type="button" className="quiet" disabled={busy} onClick={() => beginEdit("agent", agent.id, agent.name)}>Переименовать</button>
                    <button type="button" disabled={busy || issued !== null} onClick={() => rotate("agent", agent.id, agent.name)}>Перевыпустить токен</button>
                    <button type="button" className="quiet" disabled={busy} onClick={() => remove("agent", agent.id, agent.name)}>Удалить</button>
                  </div>}
                </article>;
              })}
              <AddSlot label="агента" active={isAdding({ kind: "agent", relayId: relay.id })} name={nameDraft} disabled={busy || issued !== null}
                onName={setNameDraft} onOpen={() => openAdd({ kind: "agent", relayId: relay.id })} onSubmit={submitAdd}
                onCancel={() => { setAdding(null); setNameDraft(""); }} />
            </div>
          </section>;
        })}
        {hosts.map((host) => {
          const hostEditing = isEditing("host", host.id);
          return <article className="access-host-node" key={host.id} aria-label={`Хост ${host.name}`}>
            <div className="access-node-identity">
              {hostEditing ? <NameEditor value={editing?.name ?? ""} disabled={busy} onChange={updateEditName} onSubmit={saveRename} onCancel={() => setEditing(null)} /> : <>
                <strong><HostIcon /> hwagent · {host.name}</strong><NodeStatus receivedAt={host.heartbeat?.received_at} now={now} />
                <span className="muted">Хост-агент · создан {new Date(host.created_at).toLocaleString("ru-RU")}</span>
              </>}
            </div>
            {!hostEditing && <div className="access-actions">
              <button type="button" className="quiet" disabled={busy} onClick={() => beginEdit("host", host.id, host.name)}>Переименовать</button>
              <button type="button" disabled={busy || issued !== null} onClick={() => rotate("host", host.id, host.name)}>Сменить токен</button>
              <button type="button" className="quiet" disabled={busy} onClick={() => remove("host", host.id, host.name)}>Удалить</button>
            </div>}
          </article>;
        })}
        <AddSlot label="relay" active={isAdding({ kind: "relay" })} name={nameDraft} disabled={busy || issued !== null}
          onName={setNameDraft} onOpen={() => openAdd({ kind: "relay" })} onSubmit={submitAdd}
          onCancel={() => { setAdding(null); setNameDraft(""); }} />
        <AddSlot label="hwagent" active={isAdding({ kind: "host" })} name={nameDraft} disabled={busy || issued !== null}
          onName={setNameDraft} onOpen={() => openAdd({ kind: "host" })} onSubmit={submitAdd}
          onCancel={() => { setAdding(null); setNameDraft(""); }} />
      </div>
    </section>
  </section>;
}
