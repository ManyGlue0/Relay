import { KeyRound, ShieldCheck, Upload, X } from "lucide-react";
import { useEffect, useState } from "react";
import { api, errorText, type FolderSettings } from "../lib/api";
import { folderName } from "../lib/folder";
import { useStore } from "../lib/store";
import { IconButton, Modal, Toggle } from "./ui";

/** In Auto, the title bar's shield: what Auto may do in the session's folder. */
export function AutoAccessButton() {
  const target = useStore((s) => {
    if (s.settingsOpen || (s.settings?.view ?? "chat") !== "agent") return null;
    const cur = s.current && s.current.id === s.currentId ? s.current : null;
    if (cur && cur.kind !== "agent") return null;
    const mode = (cur ? cur.mode : s.draftOptions.mode) ?? s.settings?.agentMode ?? "ask";
    const folder = cur ? cur.folder : (s.draftOptions.folder ?? null);
    return mode === "auto" && folder ? folder : null;
  });
  const [open, setOpen] = useState(false);
  if (!target) return null;
  return (
    <>
      <IconButton label="What Auto may do in this folder" active={open} className="titlebar-access" onClick={() => setOpen(true)}>
        <ShieldCheck size={16} />
      </IconButton>
      <AutoAccessSheet folder={target} open={open} onClose={() => setOpen(false)} />
    </>
  );
}

/** Push and publish, and which secret environment variables commands may see. */
export function AutoAccessSheet({ folder, open, onClose }: { folder: string; open: boolean; onClose: () => void }) {
  const [settings, setSettings] = useState<FolderSettings | null>(null);
  useEffect(() => {
    if (!open) return;
    void api.folderSettings(folder).then(setSettings).catch((e) => useStore.getState().toast(errorText(e)));
  }, [open, folder]);
  const change = (c: { autoPush?: boolean; secret?: string; allow?: boolean }) =>
    void api
      .setFolderSettings(folder, c)
      .then(setSettings)
      .catch((e) => useStore.getState().toast(errorText(e)));
  return (
    <Modal open={open} onClose={onClose} width={460}>
      <div className="sheet access-sheet">
        <div className="sheet-head">
          <h3>What Auto may do in {folderName(folder)}</h3>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <X size={16} />
          </button>
        </div>
        <p className="sheet-text">Auto runs steps without asking and refuses the risky ones. These are the exceptions you allow, for this folder only.</p>
        {settings && (
          <>
            <div className="access-group">
              <div className="access-row">
                <span className="access-icon">
                  <Upload size={15} />
                </span>
                <span className="access-text">
                  <span className="access-label">Push and publish</span>
                  <span className="access-hint">git push, gh pr create, npm publish and the like run instead of being refused. Force pushes stay refused.</span>
                </span>
                <Toggle label="Push and publish" on={settings.autoPush} onChange={(autoPush) => change({ autoPush })} />
              </div>
            </div>
            <div className="access-title">Secrets commands can see</div>
            <p className="access-intro">Environment variables that look like keys, tokens or passwords are hidden from commands. Let one through when a command needs it: its value stays hidden in what the model reads.</p>
            <div className="access-group">
              {settings.secrets.length === 0 && <div className="access-none">Relay's environment has no secret variables.</div>}
              {settings.secrets.map((x) => (
                <div key={x.name} className="access-row">
                  <span className="access-icon">
                    <KeyRound size={15} />
                  </span>
                  <span className="access-text">
                    <span className="access-label mono">{x.name}</span>
                    <span className="access-hint">{x.allowed ? "Commands see it" : "Hidden from commands"}</span>
                  </span>
                  <Toggle label={`Let commands see ${x.name}`} on={x.allowed} onChange={(allow) => change({ secret: x.name, allow })} />
                </div>
              ))}
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}
