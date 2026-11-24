import { FolderClosed, X } from "lucide-react";
import { api } from "../../lib/api";
import { folderName } from "../../lib/folder";
import { useStore } from "../../lib/store";
import { IconButton, Stepper, Toggle } from "../ui";
import { Group, Page, Row } from "./kit";

/** Agent mode: the shells sessions get, how much checkpoints keep, and the
 *  folders with their own Auto settings. */
export function Agent() {
  const s = useStore((st) => st.settings);
  const update = useStore((st) => st.updateSettings);
  if (!s) return null;
  const folders = Object.entries(s.folders ?? {}).filter(([, f]) => f.autoPush || f.allowSecrets.length > 0);

  return (
    <Page title="Agent" intro="Agent sessions work in a folder: they read, write and run commands there, within the permission mode you pick in the composer.">
      <Group title="Commands">
        <Row label="Give sessions a cmd.exe tool" hint="PowerShell is always there, and Git Bash when it's installed. cmd.exe is for the few tools that need it">
          <Toggle label="Give sessions a cmd.exe tool" on={s.agentCmd} onChange={(agentCmd) => void update({ agentCmd })} />
        </Row>
      </Group>

      <Group title="Checkpoints" hint="Recorded before every change, so any step can be undone">
        <Row label="Largest file protected" hint="Bigger files aren't recorded (the files panel lists them)">
          <Stepper value={s.checkpointFileMb} min={5} max={500} step={5} format={(v) => `${v} MB`} onChange={(checkpointFileMb) => void update({ checkpointFileMb })} />
        </Row>
        <Row label="Space for each folder" hint="Past it, older sessions keep only the last 5 versions of each file">
          <Stepper value={s.checkpointBudgetGb} min={1} max={50} format={(v) => `${v} GB`} onChange={(checkpointBudgetGb) => void update({ checkpointBudgetGb })} />
        </Row>
      </Group>

      <Group title="Folders" hint="What Auto may do there, set from the composer">
        {folders.length === 0 && <Row label="No folder has its own settings yet" hint="In Auto, the folder chip has &quot;Auto may push and publish&quot;, and Manage lets hidden secrets through" />}
        {folders.map(([key, f]) => (
          <Row
            key={key}
            label={
              <span className="agent-folder">
                <FolderClosed size={14} />
                <span title={key}>{folderName(key)}</span>
              </span>
            }
            hint={[f.autoPush ? "Auto may push and publish" : "", f.allowSecrets.length ? `Secrets let through: ${f.allowSecrets.join(", ")}` : ""].filter(Boolean).join(" · ")}
          >
            <IconButton
              size="small"
              label="Forget this folder's settings"
              onClick={() => {
                void (async () => {
                  await api.setFolderSettings(key, { autoPush: false });
                  for (const name of f.allowSecrets) await api.setFolderSettings(key, { secret: name, allow: false });
                  useStore.setState({ settings: await api.getSettings() });
                })();
              }}
            >
              <X size={14} />
            </IconButton>
          </Row>
        ))}
      </Group>
    </Page>
  );
}
