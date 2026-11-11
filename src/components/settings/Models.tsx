import { ChevronLeft, ChevronRight, CircleAlert, CircleCheck, Eye, EyeOff, KeyRound, Loader, Plug, Plus, Search, Trash2, X } from "lucide-react";
import { useState } from "react";
import { api, EFFORTS, effortOf, errorText, type Provider } from "../../lib/api";
import { compact, providerModels, shortModel, useStore } from "../../lib/store";
import { ModelPicker } from "../ModelPicker";
import { IconGlyph, IconPicker, ProviderIcon, autoIcon, iconOf } from "../ProviderIcon";
import { Button, Empty, Select, Toggle } from "../ui";
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

const PRESETS: { name: string; url: string; icon: string }[] = [
  { name: "Ollama", url: "http://localhost:11434/v1", icon: "brand:ollama" },
  { name: "LM Studio", url: "http://localhost:1234/v1", icon: "brand:lmstudio" },
  { name: "llama.cpp", url: "http://localhost:8080/v1", icon: "lucide:cpu" },
  { name: "vLLM", url: "http://localhost:8000/v1", icon: "brand:vllm" },
  { name: "OpenAI", url: "https://api.openai.com/v1", icon: "brand:openai" },
  { name: "Anthropic", url: "https://api.anthropic.com/v1", icon: "brand:anthropic" },
  { name: "SpaceXAI", url: "https://api.x.ai/v1", icon: "brand:grok" },
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

  const runTest = async (): Promise<string[] | null> => {
    if (!url.trim()) return null;
    setTest({ state: "busy" });
    try {
      const found = await api.testProvider(url, keyArg || undefined, keyTouched ? undefined : provider?.id);
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
      const p = await api.saveProvider({ id: provider?.id, name, baseUrl: url, customModels: custom, showAll, icon: icon ?? "", key: keyArg, models: found ?? undefined, contexts: ctx });
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
          <Field label="Address" hint="The OpenAI-compatible base address, usually ending in /v1">
            <input
              className="input"
              value={url}
              placeholder="http://localhost:11434/v1"
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
