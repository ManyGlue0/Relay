import { AudioLines, ChevronLeft, ChevronRight, CircleAlert, CircleCheck, Eye, EyeOff, FileText, Film, Image, KeyRound, Loader, Plug, Plus, RefreshCw, Search, Trash2, Wrench, X, type LucideIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { api, EFFORTS, effortOf, errorText, type Ability, type Provider, type ProviderApi } from "../../lib/api";
import { compact, providerModels, shortModel, useStore } from "../../lib/store";
import { ModelPicker } from "../ModelPicker";
import { IconGlyph, IconPicker, ProviderIcon, autoIcon, iconOf } from "../ProviderIcon";
import { Button, Empty, Segmented, Select, Toggle } from "../ui";
import { ConfirmButton, Field, Group, NumberInput, Page, Row } from "./kit";

export function Models() {
  const editing = useStore((s) => s.editing);
  const providers = useStore((s) => s.providers);
  if (editing) {
    const p = editing === "new" ? null : (providers.find((x) => x.id === editing) ?? null);
    return <ProviderEditor key={editing} provider={p} />;
  }
  return <ModelsHome />;
}

function ModelsHome() {
  const s = useStore((st) => st.settings);
  const providers = useStore((st) => st.providers);
  const presets = useStore((st) => st.presets);
  const update = useStore((st) => st.updateSettings);
  if (!s) return null;
  const add = (
    <Button variant="default" onClick={() => useStore.setState({ editing: "new" })}>
      <Plus size={15} />
      Add provider
    </Button>
  );
  return (
    <Page title="Models" action={add}>
      <Group title="New chats start with">
        <Row label="Model">
          {providers.length ? (
            <ModelPicker value={s.defaultModel} above={false} onPick={(defaultModel) => void update({ defaultModel })} />
          ) : (
            <span className="faint">Add a provider first</span>
          )}
        </Row>
        <Row label="Thinking effort" hint={`${EFFORTS.find((e) => e.value === effortOf(s.defaultEffort))?.hint}. Servers that can't change it ignore it`}>
          <Select value={effortOf(s.defaultEffort)} width={160} onChange={(defaultEffort) => void update({ defaultEffort })} options={EFFORTS.map((e) => ({ value: e.value, label: e.label }))} />
        </Row>
        <Row label="Preset">
          <Select
            value={s.defaultPreset ?? "none"}
            onChange={(v) => void update({ defaultPreset: v === "none" ? null : v })}
            options={[{ value: "none", label: "None" }, ...presets.map((p) => ({ value: p.id, label: p.name }))]}
          />
        </Row>
      </Group>

      <Group title="Thinking">
        <Row label="Send reasoning back to the model" hint="Thinking models keep their train of thought across replies and tool calls">
          <Toggle label="Send reasoning back to the model" on={s.sendReasoning} onChange={(sendReasoning) => void update({ sendReasoning })} />
        </Row>
      </Group>

      {providers.some((p) => p.api === "anthropic") && (
        <Group title="Anthropic">
          <Row
            label="Keep the prompt cache for"
            hint={
              s.anthropicCache === "1h"
                ? "Saving to the cache costs twice the normal input; reading it back, a tenth. Worth it if you often come back to a chat after a few minutes"
                : "Saving to the cache costs 1.25 times the normal input; reading it back, a tenth. Best when you reply within a few minutes"
            }
          >
            <Select
              value={s.anthropicCache}
              width={160}
              onChange={(anthropicCache) => void update({ anthropicCache })}
              options={[
                { value: "5m", label: "5 minutes" },
                { value: "1h", label: "1 hour" },
              ]}
            />
          </Row>
        </Group>
      )}

      <Group title="Providers" hint="Each one is a server or service Relay talks to">
        {providers.length === 0 ? (
          <div className="group-empty">
            <Empty icon={<Plug size={22} />} title="No providers yet" action={<Button variant="primary" onClick={() => useStore.setState({ editing: "new" })}>Add a provider</Button>}>
              Point Relay at a local server, like Ollama or LM Studio, or at an OpenAI-compatible API.
            </Empty>
          </div>
        ) : (
          providers.map((p) => {
            const n = providerModels(p).length;
            return (
              <Row
                key={p.id}
                className="provider-row"
                onClick={() => useStore.setState({ editing: p.id })}
                label={
                  <span className="row-with-well">
                    <ProviderIcon icon={iconOf(p)} />
                    {p.name}
                  </span>
                }
                hint={`${p.baseUrl} · ${n === 1 ? "1 model" : `${n} models`}${p.hasKey ? " · key saved" : ""}`}
              >
                <ChevronRight size={16} className="row-chev" />
              </Row>
            );
          })
        )}
      </Group>
    </Page>
  );
}

/** The base address requests build on, from what was typed: the mirror of
 *  `llm::api_base`. A pasted endpoint loses its last part, an address with no
 *  path gets /v1, and so does an Anthropic address that doesn't end in a version. */
function apiBase(kind: ProviderApi, typed: string): string {
  let b = typed.trim().replace(/\/+$/, "");
  for (const end of ["/chat/completions", "/messages", "/models"]) {
    if (b.length > end.length && b.toLowerCase().endsWith(end)) {
      b = b.slice(0, -end.length).replace(/\/+$/, "");
      break;
    }
  }
  let u: URL;
  try {
    u = new URL(b);
  } catch {
    return b;
  }
  const last = u.pathname.split("/").pop() ?? "";
  const bare = u.pathname.replace(/\//g, "") === "";
  if (!u.search && (bare || (kind === "anthropic" && !/^v\d/i.test(last)))) return `${b}/v1`;
  return b;
}

/** Where a chat's requests will go, for the address field's hint. */
function endpointOf(kind: ProviderApi, typed: string): string | null {
  if (!/^https?:\/\/[^/\s]+/i.test(typed.trim())) return null;
  return apiBase(kind, typed) + (kind === "anthropic" ? "/messages" : "/chat/completions");
}

/** A plain http:// address outside this computer and the local network,
 *  where a key would travel unencrypted. */
function unencryptedRemote(address: string): boolean {
  let u: URL;
  try {
    u = new URL(address.trim());
  } catch {
    return false;
  }
  if (u.protocol !== "http:") return false;
  const h = u.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (h === "localhost" || h.endsWith(".localhost") || h.endsWith(".local") || h.endsWith(".lan") || !h.includes(".") && !h.includes(":")) return false;
  if (h === "::1" || /^f[cd]/.test(h) || h.startsWith("fe80:")) return false;
  const v4 = h.split(".").map(Number);
  if (v4.length === 4 && v4.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)) {
    const [a, b] = v4;
    return !(a === 127 || a === 10 || (a === 192 && b === 168) || (a === 172 && b >= 16 && b <= 31) || (a === 169 && b === 254) || (a === 100 && b >= 64 && b < 128));
  }
  return true;
}

const PRESETS: { name: string; url: string; icon: string; api?: ProviderApi }[] = [
  { name: "Ollama", url: "http://localhost:11434/v1", icon: "brand:ollama" },
  { name: "LM Studio", url: "http://localhost:1234/v1", icon: "brand:lmstudio" },
  { name: "llama.cpp", url: "http://localhost:8080/v1", icon: "lucide:cpu" },
  { name: "vLLM", url: "http://localhost:8000/v1", icon: "brand:vllm" },
  { name: "OpenAI", url: "https://api.openai.com/v1", icon: "brand:openai" },
  { name: "Anthropic", url: "https://api.anthropic.com/v1", icon: "brand:anthropic", api: "anthropic" },
  { name: "SpaceXSI", url: "https://api.x.ai/v1", icon: "brand:grok" },
  { name: "OpenRouter", url: "https://openrouter.ai/api/v1", icon: "brand:openrouter" },
];

type Test = { state: "idle" } | { state: "busy" } | { state: "ok"; models: string[] } | { state: "error"; message: string };

function ProviderEditor({ provider }: { provider: Provider | null }) {
  const [name, setName] = useState(provider?.name ?? "");
  const [url, setUrl] = useState(provider?.baseUrl ?? "");
  const [key, setKey] = useState("");
  const [keyTouched, setKeyTouched] = useState(false);
  const [showKey, setShowKey] = useState(false);
  const [custom, setCustom] = useState<string[]>(provider?.customModels ?? []);
  const [showAll, setShowAll] = useState(provider?.showAll ?? true);
  const [apiKind, setApiKind] = useState<ProviderApi>(provider?.api ?? "openai");
  // null: automatic (from the address); undefined in the save means "unchanged".
  const [icon, setIcon] = useState<string | null>(provider?.icon ?? null);
  const [find, setFind] = useState("");
  const [contexts, setContexts] = useState<Record<string, number>>(provider?.contexts ?? {});
  const [newModel, setNewModel] = useState("");
  const [test, setTest] = useState<Test>({ state: "idle" });
  const [saving, setSaving] = useState(false);
  const back = () => useStore.setState({ editing: null });
  const keyArg = keyTouched ? key.trim() : undefined;
  const models = test.state === "ok" ? test.models : (provider?.models ?? []);
  const allModels = [...new Set([...models, ...custom])];
  const endpoint = endpointOf(apiKind, url);

  const runTest = async (): Promise<string[] | null> => {
    if (!url.trim()) return null;
    setTest({ state: "busy" });
    try {
      const found = await api.testProvider(url, keyArg || undefined, keyTouched ? undefined : provider?.id, apiKind);
      setTest({ state: "ok", models: found });
      return found;
    } catch (e) {
      setTest({ state: "error", message: errorText(e) });
      return null;
    }
  };

  const save = async () => {
    setSaving(true);
    try {
      const found = test.state === "ok" ? test.models : await runTest();
      // Context windows: what's set here wins; an empty box means "detect".
      const ctx: Record<string, number> = {};
      for (const m of allModels) ctx[m] = contexts[m] ?? 0;
      const p = await api.saveProvider({ id: provider?.id, name, baseUrl: url, customModels: custom, showAll, icon: icon ?? "", api: apiKind, key: keyArg, models: found ?? undefined, contexts: ctx });
      await useStore.getState().refreshProviders();
      useStore.setState({ contexts: {} });
      if (!found && !custom.length) {
        useStore.setState({ editing: p.id });
        useStore.getState().toast("Saved. Relay couldn't list its models yet");
      } else {
        back();
      }
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  const addModel = () => {
    const m = newModel.trim();
    if (m && !custom.includes(m)) setCustom([...custom, m]);
    setNewModel("");
  };

  const backButton = (
    <button className="back" onClick={back}>
      <ChevronLeft size={16} />
      <span>Models</span>
    </button>
  );

  return (
    <Page title={provider ? provider.name : "Add a provider"} action={backButton}>
      {!provider && (
        <Group title="Start from">
          <div className="presets">
            {PRESETS.map((p) => (
              <button
                key={p.name}
                className={`preset-chip ${url === p.url ? "on" : ""}`}
                onClick={() => {
                  setName(p.name);
                  setUrl(p.url);
                  setIcon(p.icon);
                  setApiKind(p.api ?? "openai");
                  setTest({ state: "idle" });
                }}
              >
                <IconGlyph icon={p.icon} size={13} />
                {p.name}
              </button>
            ))}
          </div>
        </Group>
      )}

      <Group title="Connection">
        <div className="form">
          <div className="field">
            <span className="field-label">Name and icon</span>
            <div className="name-row">
              <IconPicker value={icon} auto={autoIcon(url, name)} onChange={setIcon} />
              <input className="input" value={name} placeholder="My server" aria-label="Name" onChange={(e) => setName(e.target.value)} />
            </div>
          </div>
          <Field label="API" hint={apiKind === "anthropic" ? "The Messages API of Anthropic and services compatible with it: Claude's thinking, prompt caching and web search work fully" : "The Chat Completions API most servers speak"}>
            <Segmented
              id="provider-api"
              value={apiKind}
              options={[
                { value: "openai", label: "OpenAI-compatible" },
                { value: "anthropic", label: "Anthropic" },
              ]}
              onChange={(v) => {
                setApiKind(v);
                setTest({ state: "idle" });
              }}
            />
          </Field>
          <Field
            label="Address"
            hint={
              endpoint ? (
                <>
                  Requests go to <span className="endpoint">{endpoint}</span>
                </>
              ) : apiKind === "anthropic" ? (
                "From the service's docs, with or without /v1/messages at the end"
              ) : (
                "From the server's docs, usually ending in /v1 (a full address ending in /chat/completions works too)"
              )
            }
          >
            <input
              className="input"
              value={url}
              placeholder={apiKind === "anthropic" ? "https://api.anthropic.com/v1" : "http://localhost:11434/v1"}
              spellCheck={false}
              onChange={(e) => {
                setUrl(e.target.value);
                setTest({ state: "idle" });
              }}
            />
          </Field>
          <Field
            label="API key"
            hint={
              <>
                Kept in Windows Credential Manager, never in Relay's files.
                {provider?.hasKey && !keyTouched && (
                  <>
                    {" "}
                    <button
                      type="button"
                      className="link"
                      onClick={() => {
                        setKey("");
                        setKeyTouched(true);
                      }}
                    >
                      Remove the saved key
                    </button>
                  </>
                )}
                {provider?.hasKey && keyTouched && !key && " The saved key will be removed."}
                {(keyTouched ? !!key.trim() : !!provider?.hasKey) && unencryptedRemote(url) && " This address isn't encrypted (http://), so the key would cross the internet readable: use https:// if the server has it."}
              </>
            }
          >
            <div className="key-input">
              <KeyRound size={14} className="key-icon" />
              <input
                className="input"
                type={showKey ? "text" : "password"}
                value={key}
                spellCheck={false}
                placeholder={provider?.hasKey && !keyTouched ? "Saved (type to replace)" : "Not needed for most local servers"}
                onChange={(e) => {
                  setKey(e.target.value);
                  setKeyTouched(true);
                  setTest({ state: "idle" });
                }}
              />
              {key && (
                <button type="button" className="key-eye" aria-label={showKey ? "Hide key" : "Show key"} onClick={() => setShowKey(!showKey)}>
                  {showKey ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
              )}
            </div>
          </Field>
          <div className="test-row">
            <Button variant="default" onClick={() => void runTest()} disabled={!url.trim() || test.state === "busy"}>
              {test.state === "busy" ? <Loader size={14} className="spin" /> : <Plug size={14} />}
              Test connection
            </Button>
            <TestResult test={test} />
          </div>
        </div>
      </Group>

      <Group title="Models" hint={showAll ? "Listed by the server, plus any you add" : "Only the ones you pick, plus any you add"}>
        <Row
          label="Show every model the server has"
          hint={showAll ? "New models show up by themselves" : "Turn on the ones you want in the model picker"}
        >
          <Toggle label="Show every model the server has" on={showAll} onChange={setShowAll} />
        </Row>
        {!showAll && allModels.length > 8 && (
          <div className="model-find">
            <Search size={14} />
            <input value={find} placeholder={`Find among ${allModels.length} models`} spellCheck={false} onChange={(e) => setFind(e.target.value)} />
          </div>
        )}
        {allModels
          .filter((m) => showAll || !find.trim() || m.toLowerCase().includes(find.trim().toLowerCase()))
          .map((m) => {
            const used = showAll || custom.includes(m);
            const listed = models.includes(m);
            return (
              <Row key={m} className={used ? "" : "row-off"} label={<span className="mono-label">{shortModel(m)}</span>} hint={m.includes("/") ? m : undefined}>
                {used && (
                  <>
                    <span className="ctx-label">Context</span>
                    <NumberInput
                      value={contexts[m] ?? null}
                      placeholder="Detect"
                      min={256}
                      max={10_000_000}
                      width={96}
                      onChange={(v) =>
                        setContexts((c) => {
                          const next = { ...c };
                          if (v) next[m] = Math.round(v);
                          else delete next[m];
                          return next;
                        })
                      }
                    />
                  </>
                )}
                {showAll ? (
                  custom.includes(m) &&
                  !listed && (
                    <button className="chip-x" aria-label={`Remove ${m}`} onClick={() => setCustom(custom.filter((x) => x !== m))}>
                      <X size={13} />
                    </button>
                  )
                ) : (
                  <Toggle label={`Use ${m}`} on={used} onChange={(on) => setCustom((c) => (on ? [...c.filter((x) => x !== m), m] : c.filter((x) => x !== m)))} />
                )}
              </Row>
            );
          })}
        <Row label="Add a model by name" hint="For servers that don't list their models: the exact model id">
          <input
            className="input"
            style={{ width: 200 }}
            value={newModel}
            placeholder="e.g. gpt-4o-mini"
            spellCheck={false}
            onChange={(e) => setNewModel(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addModel();
              }
            }}
          />
          <Button variant="default" onClick={addModel} disabled={!newModel.trim()}>
            Add
          </Button>
        </Row>
      </Group>
      {provider && <Abilities providerId={provider.id} models={allModels.filter((m) => showAll || custom.includes(m))} />}
      {allModels.some((m) => provider?.contexts[m]) && (
        <p className="set-note">
          Detected so far:{" "}
          {allModels
            .filter((m) => provider?.contexts[m])
            .map((m) => `${shortModel(m)} ${compact(provider!.contexts[m])}`)
            .join(", ")}
        </p>
      )}

      <div className="form-actions">
        {provider && (
          <ConfirmButton
            label={
              <>
                <Trash2 size={14} />
                Remove
              </>
            }
            question={`Remove ${provider.name}?`}
            confirm="Remove"
            onConfirm={async () => {
              await api.deleteProvider(provider.id);
              await useStore.getState().refreshProviders();
              back();
            }}
          />
        )}
        <div className="form-fill" />
        <Button variant="ghost" onClick={back}>
          Cancel
        </Button>
        <Button variant="primary" onClick={() => void save()} disabled={!name.trim() || !url.trim() || saving}>
          {saving && <Loader size={14} className="spin" />}
          {provider ? "Save" : "Add provider"}
        </Button>
      </div>
    </Page>
  );
}

const ABILITIES: { key: Ability; label: string; icon: LucideIcon; what: string }[] = [
  { key: "vision", label: "Images", icon: Image, what: "sees images" },
  { key: "audio", label: "Audio", icon: AudioLines, what: "hears audio" },
  { key: "video", label: "Video", icon: Film, what: "watches video (else it gets stills)" },
  { key: "pdf", label: "PDFs", icon: FileText, what: "reads PDFs as files (else it gets their text)" },
  { key: "tools", label: "Tools", icon: Wrench, what: "calls tools (Agent mode needs it)" },
];
/** Rows shown before the list needs a search. */
const ABILITY_ROWS = 12;

/** What each model takes besides text: detected (or guessed), and the user can correct it. */
function Abilities({ providerId, models }: { providerId: string; models: string[] }) {
  const [find, setFind] = useState("");
  const q = find.trim().toLowerCase();
  const shown = models.filter((m) => !q || m.toLowerCase().includes(q)).slice(0, ABILITY_ROWS);
  const key = shown.join("|");
  useEffect(() => {
    for (const model of shown) useStore.getState().ensureCaps({ providerId, model });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providerId, key]);
  if (!models.length) return null;
  return (
    <Group title="What each model takes" hint="Detected from the server where it says, else guessed from the name. Click to correct it.">
      {models.length > ABILITY_ROWS && (
        <div className="model-find">
          <Search size={14} />
          <input value={find} placeholder={`Find among ${models.length} models`} spellCheck={false} onChange={(e) => setFind(e.target.value)} />
        </div>
      )}
      {shown.map((m) => (
        <AbilityRow key={m} providerId={providerId} model={m} />
      ))}
    </Group>
  );
}

function AbilityRow({ providerId, model }: { providerId: string; model: string }) {
  const c = useStore((s) => s.caps[`${providerId}/${model}`]);
  const changed = !!c && Object.keys(c.user).length > 0;
  return (
    <Row label={<span className="mono-label">{shortModel(model)}</span>} hint={changed ? "Changed by you" : undefined}>
      <div className="abilities">
        {ABILITIES.map(({ key, label, icon: Icon, what }) => {
          // Tool calling nobody checked yet counts as on (most models do), drawn as unsure.
          const unknown = key === "tools" && c?.caps.tools == null;
          const on = (key === "tools" ? c?.caps.tools !== false : c?.caps[key]) ?? false;
          const detected = key === "tools" ? c?.detected.tools !== false : c?.detected[key];
          const mine = c?.user[key] !== undefined;
          return (
            <button
              key={key}
              className={`ability ${on ? "on" : ""} ${mine ? "mine" : ""} ${unknown ? "unsure" : ""}`}
              aria-pressed={on}
              disabled={!c}
              title={unknown ? `Not checked yet: use Check to ask the model whether it ${what.split(" (")[0]}` : `${on ? "It" : "Not detected: it"} ${what}${mine ? " (you set this)" : ""}`}
              onClick={() => {
                if (!c) return;
                // Back to what was detected, or the user's word against it.
                void useStore.getState().setCap({ providerId, model }, key, !on === detected ? null : !on);
              }}
            >
              <Icon size={13} />
              {label}
            </button>
          );
        })}
        <CheckTools providerId={providerId} model={model} />
      </div>
    </Row>
  );
}

/** Asks the model whether it calls tools (one small request). */
function CheckTools({ providerId, model }: { providerId: string; model: string }) {
  const [busy, setBusy] = useState(false);
  return (
    <button
      className="ability check"
      disabled={busy}
      title="Ask the model whether it calls tools (one small request)"
      onClick={async () => {
        setBusy(true);
        await useStore.getState().checkTools({ providerId, model });
        setBusy(false);
      }}
    >
      {busy ? <Loader size={13} className="spin" /> : <RefreshCw size={13} />}
      Check
    </button>
  );
}

function TestResult({ test }: { test: Test }) {
  if (test.state === "ok")
    return (
      <span className="test-ok">
        <CircleCheck size={14} />
        {test.models.length === 0 ? "Connected, but it lists no models" : test.models.length === 1 ? "Connected · 1 model" : `Connected · ${test.models.length} models`}
      </span>
    );
  if (test.state === "error")
    return (
      <span className="test-error">
        <CircleAlert size={14} />
        <span>{test.message}</span>
      </span>
    );
  return null;
}
