import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { motion } from "framer-motion";
import { BookMarked, ChevronDown, ChevronLeft, ChevronRight, FileArchive, FolderOpen, Loader, Plus, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, errorText, type Skill, type SkillsInfo } from "../../lib/api";
import { spring } from "../../lib/motion";
import { useStore } from "../../lib/store";
import { anchorFrom, AutoText, Button, Empty, MenuItem, Popover, Toggle, type Anchor } from "../ui";
import { ConfirmButton, Field, Group, Page, Row } from "./kit";

const NAME_RE = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;

/** What people type, as a skill name: "PDF forms" → "pdf-forms". */
function nameOf(typed: string): string {
  return typed
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9-]/g, "")
    .replace(/-{2,}/g, "-")
    .replace(/^-/, "")
    .slice(0, 64);
}

export function Skills() {
  const editing = useStore((s) => s.editing);
  const skills = useStore((s) => s.skills);
  useEffect(() => {
    void useStore.getState().refreshSkills();
  }, [editing]);
  if (editing) {
    const skill = editing === "new" ? null : (skills.find((x) => x.id === editing) ?? null);
    return <SkillEditor key={editing} skill={skill} />;
  }
  return <SkillsHome />;
}

async function importFrom(kind: "folder" | "zip") {
  const path = await openDialog(
    kind === "folder"
      ? { directory: true, title: "Choose a skill folder (it holds a SKILL.md)" }
      : { title: "Choose a skill file", filters: [{ name: "Skill", extensions: ["zip", "skill"] }] },
  );
  if (!path || Array.isArray(path)) return;
  try {
    const r = await api.importSkill(path);
    await useStore.getState().refreshSkills();
    useStore.getState().toast(r.replaced ? `Updated ${r.skill.name}` : `Added ${r.skill.name}`);
  } catch (e) {
    useStore.getState().toast(errorText(e));
  }
}

function SkillsHome() {
  const skills = useStore((s) => s.skills);
  const [info, setInfo] = useState<SkillsInfo | null>(null);
  const [menu, setMenu] = useState<Anchor | null>(null);
  const addRef = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    void api.skillsInfo().then(setInfo);
  }, [skills]);

  const actions = (
    <div className="head-buttons">
      <span ref={addRef}>
        <Button variant="ghost" onClick={() => setMenu(menu ? null : anchorFrom(addRef.current!, { alignRight: true }))}>
          Add
          <ChevronDown size={14} />
        </Button>
      </span>
      <Button variant="default" onClick={() => useStore.setState({ editing: "new" })}>
        <Plus size={15} />
        New skill
      </Button>
      <Popover anchor={menu} onClose={() => setMenu(null)} width={220} trigger={addRef}>
        <div className="menu">
          <MenuItem
            icon={<FolderOpen size={15} />}
            onSelect={() => {
              setMenu(null);
              void importFrom("folder");
            }}
          >
            From a folder…
          </MenuItem>
          <MenuItem
            icon={<FileArchive size={15} />}
            onSelect={() => {
              setMenu(null);
              void importFrom("zip");
            }}
          >
            From a .zip file…
          </MenuItem>
        </div>
      </Popover>
    </div>
  );

  return (
    <Page
      title="Skills"
      action={actions}
      intro="Skills teach models particular tasks: instructions, plus the files and scripts that go with them. Models see each skill's name and description, and load the rest when a task calls for it. Skills work in chats that use tools."
    >
      <Group title="Your skills">
        {skills.length === 0 ? (
          <div className="group-empty">
            <Empty icon={<BookMarked size={22} />} title="No skills yet" action={<Button variant="primary" onClick={() => useStore.setState({ editing: "new" })}>Write a skill</Button>}>
              Write one here, or add a skill folder or .zip: a folder with a <code>SKILL.md</code>, in the Agent Skills format.
            </Empty>
          </div>
        ) : (
          skills.map((s) => (
            <Row
              key={s.id}
              onClick={() => useStore.setState({ editing: s.id })}
              label={<span className="mono-label">{s.name}</span>}
              hint={s.error ? <span className="hint-error">{s.error}</span> : <span className="clamp-2">{s.description}</span>}
            >
              <span onClick={(e) => e.stopPropagation()}>
                <Toggle
                  label={`Use ${s.name}`}
                  on={s.enabled && !s.error}
                  onChange={async (enabled) => {
                    await api.setSkill(s.id, { enabled });
                    await useStore.getState().refreshSkills();
                  }}
                />
              </span>
              <ChevronRight size={16} className="row-chev" />
            </Row>
          ))
        )}
      </Group>

      <Group title="Folders">
        <Row label="Skills" hint={info?.folder}>
          <Button variant="default" onClick={() => void api.openSkillsFolder("")}>
            <FolderOpen size={14} />
            Open
          </Button>
        </Row>
        <Row label="Script workspace" hint="Scripts run here, and what they make lands here">
          <Button variant="default" onClick={() => void api.openSkillsFolder("workspace")}>
            <FolderOpen size={14} />
            Open
          </Button>
        </Row>
      </Group>

      {skills.length > 0 && <Disclosure label="What models see" text={info?.prompt || "Nothing: every skill is off."} />}
    </Page>
  );
}

function Disclosure({ label, text }: { label: string; text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <section className="group">
      <button className="disclosure" onClick={() => setOpen(!open)}>
        <motion.span animate={{ rotate: open ? 90 : 0 }} transition={spring} className="disclosure-chev">
          <ChevronRight size={14} />
        </motion.span>
        {label}
      </button>
      {open && <pre className="prompt-preview">{text}</pre>}
    </section>
  );
}

function SkillEditor({ skill }: { skill: Skill | null }) {
  const [name, setName] = useState(skill?.name ?? "");
  const [description, setDescription] = useState(skill?.description ?? "");
  const [body, setBody] = useState(skill?.body ?? "");
  const [saving, setSaving] = useState(false);
  const live = useStore((s) => s.skills.find((x) => x.id === skill?.id)) ?? skill;
  const back = () => useStore.setState({ editing: null });
  const nameOk = NAME_RE.test(name);

  const save = async () => {
    setSaving(true);
    try {
      await api.saveSkill(skill?.id ?? null, name, description, body);
      await useStore.getState().refreshSkills();
      back();
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Page
      title={skill ? skill.name : "New skill"}
      action={
        <button className="back" onClick={back}>
          <ChevronLeft size={16} />
          <span>Skills</span>
        </button>
      }
    >
      {live?.error && (
        <Group>
          <Row label={<span className="hint-error">{live.error}</span>} />
        </Group>
      )}

      {live && (
        <Group>
          <Row label="Use this skill" hint="Off: models don't see it">
            <Toggle
              label="Use this skill"
              on={live.enabled}
              onChange={async (enabled) => {
                await api.setSkill(live.id, { enabled });
                await useStore.getState().refreshSkills();
              }}
            />
          </Row>
        </Group>
      )}

      <Group>
        <div className="form">
          <Field label="Name" hint={name && !nameOk ? <span className="hint-error">Lowercase letters, digits and hyphens, without a hyphen at either end</span> : "Lowercase letters, digits and hyphens. Models load the skill by this name"}>
            <input className="input mono" value={name} autoFocus={!skill} spellCheck={false} placeholder="e.g. meeting-notes" onChange={(e) => setName(nameOf(e.target.value))} />
          </Field>
          <Field label="Description" hint="What it does and when to use it. Models read only this until they load the skill">
            <AutoText value={description} onChange={setDescription} minRows={2} placeholder="Turns rough meeting notes into a summary with decisions and action items. Use when the user shares notes from a meeting." />
          </Field>
          <Field label="Instructions" hint="Markdown. What the model reads when it loads the skill: steps, rules, examples">
            <AutoText value={body} onChange={setBody} minRows={12} maxHeight={640} className="mono" placeholder={"# Meeting notes\n\n1. List the decisions first, one line each.\n2. Then the action items as a checklist, with owners.\n3. Keep it under a page."} />
          </Field>
        </div>
      </Group>

      {live && live.files.length > 0 && (
        <Group
          title="Files"
          hint="Models read these when the instructions point to them"
          action={
            <button className="link-btn" onClick={() => void api.openSkillsFolder(live.id)}>
              Open folder
            </button>
          }
        >
          {live.files.map((f) => (
            <Row key={f} label={<span className="mono-label">{f}</span>} hint={live.scripts.includes(f) ? "Script" : undefined} />
          ))}
        </Group>
      )}

      {live && live.scripts.length > 0 && (
        <Group title="Scripts">
          <Row label="Run scripts without asking" hint={live.scriptsAllowed ? "This skill's scripts run as soon as a model asks" : "Relay asks you before every run"}>
            <Toggle
              label="Run scripts without asking"
              on={live.scriptsAllowed}
              onChange={async (scriptsAllowed) => {
                await api.setSkill(live.id, { scriptsAllowed });
                await useStore.getState().refreshSkills();
              }}
            />
          </Row>
        </Group>
      )}

      {live && !live.files.length && (
        <Group>
          <Row label="Files" hint="A skill can bring references, templates and scripts: put them in its folder">
            <Button variant="default" onClick={() => void api.openSkillsFolder(live.id)}>
              <FolderOpen size={14} />
              Open folder
            </Button>
          </Row>
        </Group>
      )}

      <div className="form-actions">
        {skill && (
          <ConfirmButton
            label={
              <>
                <Trash2 size={14} />
                Remove
              </>
            }
            question={`Remove ${skill.name} and its folder?`}
            confirm="Remove"
            onConfirm={async () => {
              try {
                await api.deleteSkill(skill.id);
                await useStore.getState().refreshSkills();
                back();
              } catch (e) {
                useStore.getState().toast(errorText(e));
              }
            }}
          />
        )}
        <div className="form-fill" />
        <Button variant="ghost" onClick={back}>
          Cancel
        </Button>
        <Button variant="primary" onClick={() => void save()} disabled={!nameOk || !description.trim() || saving}>
          {saving && <Loader size={14} className="spin" />}
          {skill ? "Save" : "Create skill"}
        </Button>
      </div>
    </Page>
  );
}
