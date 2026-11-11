import { ChevronLeft, ChevronRight, Loader, Plus, Settings2, Trash2 } from "lucide-react";
import { useState } from "react";
import { api, EFFORTS, errorText, type Effort, type Preset } from "../../lib/api";
import { useStore } from "../../lib/store";
import { AutoText, Button, Empty, Select } from "../ui";
import { ConfirmButton, Field, Group, NumberInput, Page, Row } from "./kit";

export function Presets() {
  const editing = useStore((s) => s.editing);
  const presets = useStore((s) => s.presets);
  if (editing) {
    const p = editing === "new" ? null : (presets.find((x) => x.id === editing) ?? null);
    return <PresetEditor key={editing} preset={p} />;
  }
  const defaultPreset = useStore.getState().settings?.defaultPreset;
  const add = (
    <Button variant="default" onClick={() => useStore.setState({ editing: "new" })}>
      <Plus size={15} />
      New preset
    </Button>
  );
  return (
    <Page title="Presets" action={add} intro="A preset is a system prompt with its own settings: a coding helper, a translator, a tutor. Pick one per chat from the composer.">
      {presets.length === 0 ? (
        <Group>
          <div className="group-empty">
            <Empty icon={<Settings2 size={22} />} title="No presets yet" action={<Button variant="primary" onClick={() => useStore.setState({ editing: "new" })}>Create a preset</Button>}>
              Save the instructions you'd otherwise paste into every chat.
            </Empty>
          </div>
        </Group>
      ) : (
        <Group>
          {presets.map((p) => (
            <Row key={p.id} onClick={() => useStore.setState({ editing: p.id })} label={p.name} hint={summary(p, p.id === defaultPreset)}>
              <ChevronRight size={16} className="row-chev" />
            </Row>
          ))}
        </Group>
      )}
    </Page>
  );
}

function summary(p: Preset, isDefault: boolean): string {
  const bits: string[] = [];
  if (isDefault) bits.push("For new chats");
  const first = p.systemPrompt.trim().split("\n")[0];
  if (first) bits.push(first.length > 70 ? `${first.slice(0, 70)}…` : first);
  if (p.temperature != null) bits.push(`temperature ${p.temperature}`);
  if (p.effort) bits.push(`${EFFORTS.find((e) => e.value === p.effort)?.label.toLowerCase()} effort`);
  return bits.join(" · ") || "No instructions yet";
}

function PresetEditor({ preset }: { preset: Preset | null }) {
  const [name, setName] = useState(preset?.name ?? "");
  const [prompt, setPrompt] = useState(preset?.systemPrompt ?? "");
  const [temperature, setTemperature] = useState<number | null>(preset?.temperature ?? null);
  const [topP, setTopP] = useState<number | null>(preset?.topP ?? null);
  const [maxTokens, setMaxTokens] = useState<number | null>(preset?.maxTokens ?? null);
  const [effort, setEffort] = useState<Effort | "chat">(preset?.effort ?? "chat");
  const [saving, setSaving] = useState(false);
  const isDefault = useStore((s) => !!preset && s.settings?.defaultPreset === preset.id);
  const back = () => useStore.setState({ editing: null });

  const save = async () => {
    setSaving(true);
    try {
      await api.savePreset({
        id: preset?.id ?? "",
        name,
        systemPrompt: prompt,
        temperature,
        topP,
        maxTokens: maxTokens == null ? null : Math.round(maxTokens),
        effort: effort === "chat" ? null : effort,
        sort: preset?.sort ?? 0,
      });
      await useStore.getState().refreshPresets();
      back();
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Page
      title={preset ? preset.name : "New preset"}
      action={
        <button className="back" onClick={back}>
          <ChevronLeft size={16} />
          <span>Presets</span>
        </button>
      }
    >
      <Group>
        <div className="form">
          <Field label="Name">
            <input className="input" value={name} autoFocus={!preset} placeholder="e.g. Code reviewer" onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Instructions" hint="The system prompt. It comes after what you set in You, if you share that">
            <AutoText value={prompt} onChange={setPrompt} minRows={6} placeholder="You review code for bugs first, then style. Quote the lines you mean." />
          </Field>
        </div>
      </Group>

      <Group title="Settings" hint="Empty means the model's own default">
        <Row label="Temperature" hint="Lower is focused and repeatable, higher is varied (0 to 2)">
          <NumberInput value={temperature} onChange={setTemperature} min={0} max={2} step={0.1} />
        </Row>
        <Row label="Top P" hint="Sample from the most likely words only (0 to 1)">
          <NumberInput value={topP} onChange={setTopP} min={0} max={1} step={0.05} />
        </Row>
        <Row label="Longest reply" hint="In tokens">
          <NumberInput value={maxTokens} onChange={setMaxTokens} min={1} max={1_000_000} />
        </Row>
        <Row label="Thinking effort" hint={effort === "chat" ? "Uses the chat's (or the default) effort" : EFFORTS.find((e) => e.value === effort)?.hint}>
          <Select
            value={effort}
            onChange={setEffort}
            width={180}
            options={[{ value: "chat" as const, label: "The chat's" }, ...EFFORTS.map((e) => ({ value: e.value, label: e.label }))]}
          />
        </Row>
      </Group>

      {preset && (
        <Group>
          <Row label="Use for new chats" hint="New chats start with this preset">
            <Button variant="default" disabled={isDefault} onClick={() => void useStore.getState().updateSettings({ defaultPreset: preset.id })}>
              {isDefault ? "It's the default" : "Make it the default"}
            </Button>
          </Row>
        </Group>
      )}

      <div className="form-actions">
        {preset && (
          <ConfirmButton
            label={
              <>
                <Trash2 size={14} />
                Delete
              </>
            }
            question={`Delete ${preset.name}?`}
            confirm="Delete"
            onConfirm={async () => {
              await api.deletePreset(preset.id);
              await useStore.getState().refreshPresets();
              const settings = await api.getSettings();
              useStore.setState({ settings });
              back();
            }}
          />
        )}
        <div className="form-fill" />
        <Button variant="ghost" onClick={back}>
          Cancel
        </Button>
        <Button variant="primary" onClick={() => void save()} disabled={!name.trim() || saving}>
          {saving && <Loader size={14} className="spin" />}
          {preset ? "Save" : "Create preset"}
        </Button>
      </div>
    </Page>
  );
}
