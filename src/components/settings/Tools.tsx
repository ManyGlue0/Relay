import { ChevronLeft, ChevronRight, FileJson, Loader, Plus, RotateCcw, Trash2, Wrench, X } from "lucide-react";
import { useEffect, useState } from "react";
import { api, errorText, type McpView, type ToolPolicy } from "../../lib/api";
import { useStore } from "../../lib/store";
import { AutoText, Button, Dot, Empty, Modal, Segmented, Stepper, Toggle } from "../ui";
import { ConfirmButton, Field, Group, Page, Row } from "./kit";

const POLICIES: { value: ToolPolicy; label: string }[] = [
  { value: "ask", label: "Ask" },
  { value: "allow", label: "Allow" },
  { value: "off", label: "Off" },
];

function dotOf(s: McpView): "ok" | "busy" | "off" | "error" {
  if (!s.enabled) return "off";
  return s.status.state === "ready" ? "ok" : s.status.state === "connecting" ? "busy" : s.status.state === "error" ? "error" : "off";
}

function statusLine(s: McpView): string {
  if (!s.enabled) return "Off";
  switch (s.status.state) {
    case "ready":
      return `${s.status.tools.length === 1 ? "1 tool" : `${s.status.tools.length} tools`}${s.status.serverName ? ` · ${s.status.serverName}${s.status.serverVersion ? ` ${s.status.serverVersion}` : ""}` : ""}`;
    case "connecting":
      return "Connecting…";
    case "error":
      return s.status.error ?? "Not connected";
    default:
      return "Not connected";
  }
}

export function Tools() {
  const editing = useStore((s) => s.editing);
  const servers = useStore((s) => s.mcp);
  useEffect(() => {
    void useStore.getState().refreshMcp();
  }, [editing]);
  if (editing) {
    const s = editing === "new" ? null : (servers.find((x) => x.id === editing) ?? null);
    return <ServerEditor key={editing} server={s} />;
  }
  return <ToolsHome />;
}

function ToolsHome() {
  const servers = useStore((s) => s.mcp);
  const rounds = useStore((s) => s.settings?.toolRounds ?? 12);
  const [importing, setImporting] = useState(false);
  const actions = (
    <div className="head-buttons">
      <Button variant="ghost" onClick={() => setImporting(true)}>
        <FileJson size={15} />
        Import
      </Button>
      <Button variant="default" onClick={() => useStore.setState({ editing: "new" })}>
        <Plus size={15} />
        Add server
      </Button>
    </div>
  );
  return (
    <Page title="Tools" action={actions} intro="Give models tools through MCP servers: files, search, your apps. Relay asks before every tool call unless you allow a tool.">
      <Group title="Servers">
        {servers.length === 0 ? (
          <div className="group-empty">
            <Empty icon={<Wrench size={22} />} title="No tool servers yet" action={<Button variant="primary" onClick={() => useStore.setState({ editing: "new" })}>Add a server</Button>}>
              Add an MCP server by its command (like <code>npx -y @modelcontextprotocol/server-filesystem</code>) or its address, or import a Claude Desktop config.
            </Empty>
          </div>
        ) : (
          servers.map((s) => (
            <Row
              key={s.id}
              onClick={() => useStore.setState({ editing: s.id })}
              label={
                <span className="row-with-well">
                  <Dot state={dotOf(s)} />
                  {s.name}
                </span>
              }
              hint={<span className={s.enabled && s.status.state === "error" ? "hint-error" : ""}>{statusLine(s)}</span>}
            >
              <span onClick={(e) => e.stopPropagation()}>
                <Toggle
                  label={`Use ${s.name}`}
                  on={s.enabled}
                  onChange={async (on) => {
                    await api.setMcpEnabled(s.id, on);
                    void useStore.getState().refreshMcp();
                  }}
                />
              </span>
              <ChevronRight size={16} className="row-chev" />
            </Row>
          ))
        )}
      </Group>

      <Group title="Limits">
        <Row label="Tool calls in a row" hint="How many rounds of tools a model may use before it has to answer">
          <Stepper value={rounds} min={1} max={50} onChange={(toolRounds) => void useStore.getState().updateSettings({ toolRounds })} />
        </Row>
      </Group>
      <ImportSheet open={importing} onClose={() => setImporting(false)} />
    </Page>
  );
}

function ImportSheet({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const run = async () => {
    try {
      const n = await api.importMcp(text);
      await useStore.getState().refreshMcp();
      useStore.getState().toast(n === 1 ? "Added 1 server" : `Added ${n} servers`);
      setText("");
      setError(null);
      onClose();
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <Modal open={open} onClose={onClose} width={560}>
      <div className="sheet">
        <div className="sheet-head">
          <h3>Import servers</h3>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <X size={16} />
          </button>
        </div>
        <p className="sheet-text">Paste the JSON from Claude Desktop, Cursor or VS Code: an object with <code>mcpServers</code>, or the servers themselves.</p>
        <AutoText value={text} onChange={setText} minRows={8} className="mono" placeholder={'{\n  "mcpServers": {\n    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:\\\\Users\\\\me\\\\Documents"] }\n  }\n}'} />
        {error && <div className="hint-error sheet-error">{error}</div>}
        <div className="form-actions">
          <div className="form-fill" />
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!text.trim()} onClick={() => void run()}>
            Import
          </Button>
        </div>
      </div>
    </Modal>
  );
}

/** Splits a command line the way people type it: spaces, with "quoted parts". */
function splitArgs(line: string): string[] {
  const out: string[] = [];
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}

function ServerEditor({ server }: { server: McpView | null }) {
  const [kind, setKind] = useState<"stdio" | "http">(server?.kind ?? "stdio");
  const [name, setName] = useState(server?.name ?? "");
  const [command, setCommand] = useState(server ? [server.command, ...server.args].filter(Boolean).map((a) => (/\s/.test(a) ? `"${a}"` : a)).join(" ") : "");
  const [cwd, setCwd] = useState(server?.cwd ?? "");
  const [url, setUrl] = useState(server?.url ?? "");
  const [env, setEnv] = useState<{ k: string; v: string }[]>([]);
  const [envLoaded, setEnvLoaded] = useState(!server);
  const [token, setToken] = useState("");
  const [tokenTouched, setTokenTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const back = () => useStore.setState({ editing: null });
  const live = useStore((s) => s.mcp.find((x) => x.id === server?.id)) ?? server;

  useEffect(() => {
    if (!server) return;
    void api.mcpEnv(server.id).then((e) => {
      setEnv(Object.entries(e).map(([k, v]) => ({ k, v })));
      setEnvLoaded(true);
    });
  }, [server]);

  const save = async () => {
    setSaving(true);
    try {
      const parts = splitArgs(command);
      await api.saveMcp(
        {
          id: server?.id ?? "",
          name,
          kind,
          command: kind === "stdio" ? (parts[0] ?? "") : "",
          args: kind === "stdio" ? parts.slice(1) : [],
          cwd: kind === "stdio" ? cwd : "",
          url: kind === "http" ? url : "",
          enabled: server?.enabled ?? true,
          sort: server?.sort ?? 0,
        },
        envLoaded ? Object.fromEntries(env.filter((e) => e.k.trim()).map((e) => [e.k.trim(), e.v])) : undefined,
        tokenTouched ? token : undefined,
      );
      await useStore.getState().refreshMcp();
      back();
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Page
      title={server ? server.name : "Add a tool server"}
      action={
        <button className="back" onClick={back}>
          <ChevronLeft size={16} />
          <span>Tools</span>
        </button>
      }
    >
      {live && (
        <Group title="Status">
          <Row label={<span className="row-with-well"><Dot state={dotOf(live)} />{statusLine(live)}</span>}>
            <Button variant="default" onClick={() => void api.restartMcp(live.id).then(() => useStore.getState().refreshMcp())}>
              <RotateCcw size={14} />
              Reconnect
            </Button>
          </Row>
        </Group>
      )}

      <Group title="Server">
        <div className="form">
          <Field label="Name" hint="Models see it as part of each tool's name">
            <input className="input" value={name} placeholder="e.g. files" onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Runs">
            <Segmented
              id="mcp-kind"
              value={kind}
              onChange={setKind}
              options={[
                { value: "stdio", label: "On this PC (command)" },
                { value: "http", label: "On a server (address)" },
              ]}
            />
          </Field>
          {kind === "stdio" ? (
            <>
              <Field label="Command" hint="The command and its arguments, as you'd type them. npx and uvx work as they are">
                <input className="input mono" value={command} spellCheck={false} placeholder="npx -y @modelcontextprotocol/server-filesystem C:\Users\me\Documents" onChange={(e) => setCommand(e.target.value)} />
              </Field>
              <Field label="Start in" hint="Optional: the folder the command runs in">
                <input className="input mono" value={cwd} spellCheck={false} placeholder="C:\Projects" onChange={(e) => setCwd(e.target.value)} />
              </Field>
            </>
          ) : (
            <>
              <Field label="Address" hint="A Streamable HTTP endpoint, usually ending in /mcp">
                <input className="input mono" value={url} spellCheck={false} placeholder="https://example.com/mcp" onChange={(e) => setUrl(e.target.value)} />
              </Field>
              <Field label="Token" hint={server?.hasToken && !tokenTouched ? "Saved in Windows Credential Manager. Type to replace it, or clear it to remove it" : "Sent as a bearer token. Kept in Windows Credential Manager"}>
                <input
                  className="input"
                  type="password"
                  value={token}
                  placeholder={server?.hasToken && !tokenTouched ? "Saved" : "Optional"}
                  onChange={(e) => {
                    setToken(e.target.value);
                    setTokenTouched(true);
                  }}
                />
              </Field>
            </>
          )}
        </div>
      </Group>

      {kind === "stdio" && (
        <Group title="Environment" hint="Kept in Windows Credential Manager">
          {env.map((e, i) => (
            <div key={i} className="env-row">
              <input className="input mono" value={e.k} placeholder="NAME" spellCheck={false} onChange={(ev) => setEnv(env.map((x, j) => (j === i ? { ...x, k: ev.target.value } : x)))} />
              <input className="input mono" type="password" value={e.v} placeholder="value" spellCheck={false} onChange={(ev) => setEnv(env.map((x, j) => (j === i ? { ...x, v: ev.target.value } : x)))} />
              <button className="chip-x" aria-label="Remove" onClick={() => setEnv(env.filter((_, j) => j !== i))}>
                <X size={13} />
              </button>
            </div>
          ))}
          <Row label={env.length ? "" : "No variables"} hint={env.length ? undefined : "API keys and settings the server reads from its environment"}>
            <Button variant="ghost" onClick={() => setEnv([...env, { k: "", v: "" }])} disabled={!envLoaded}>
              <Plus size={14} />
              Add variable
            </Button>
          </Row>
        </Group>
      )}

      {live && live.status.tools.length > 0 && <ToolList server={live} />}

      <div className="form-actions">
        {server && (
          <ConfirmButton
            label={
              <>
                <Trash2 size={14} />
                Remove
              </>
            }
            question={`Remove ${server.name}?`}
            confirm="Remove"
            onConfirm={async () => {
              await api.deleteMcp(server.id);
              await useStore.getState().refreshMcp();
              back();
            }}
          />
        )}
        <div className="form-fill" />
        <Button variant="ghost" onClick={back}>
          Cancel
        </Button>
        <Button variant="primary" onClick={() => void save()} disabled={!name.trim() || (kind === "stdio" ? !command.trim() : !url.trim()) || saving}>
          {saving && <Loader size={14} className="spin" />}
          {server ? "Save and reconnect" : "Add server"}
        </Button>
      </div>
    </Page>
  );
}

function ToolList({ server }: { server: McpView }) {
  const set = async (tool: string | null, policy: ToolPolicy) => {
    await api.setToolPolicy(server.id, tool, policy);
    await useStore.getState().refreshMcp();
  };
  return (
    <Group title="Tools" hint="Ask: Relay asks before each call. Allow: it runs on its own. Off: models don't see it">
      <Row label="All tools" hint="Sets every tool at once">
        <Segmented id={`all-${server.id}`} value={Object.keys(server.toolPolicy).length ? ("" as ToolPolicy) : server.defaultPolicy} onChange={(p) => void set(null, p)} options={POLICIES} />
      </Row>
      {server.status.tools.map((t) => (
        <Row key={t.name} label={<span className="mono-label">{t.title || t.name}</span>} hint={t.description ? (t.description.length > 160 ? `${t.description.slice(0, 160)}…` : t.description) : undefined}>
          <Segmented id={`tool-${server.id}-${t.name}`} value={server.toolPolicy[t.name] ?? server.defaultPolicy} onChange={(p) => void set(t.name, p)} options={POLICIES} />
        </Row>
      ))}
    </Group>
  );
}
