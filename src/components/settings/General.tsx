import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { api, type HolsterStatus } from "../../lib/api";
import { useStore } from "../../lib/store";
import { TitleModelPicker } from "../ModelPicker";
import { Button, Dot, Segmented, Stepper, Toggle } from "../ui";
import { ConfirmButton, Group, Page, Row } from "./kit";

export function General() {
  const s = useStore((st) => st.settings);
  const update = useStore((st) => st.updateSettings);
  const [dataDir, setDataDir] = useState("");
  useEffect(() => {
    void api.appInfo().then((i) => setDataDir(i.dataDir));
  }, []);
  if (!s) return null;

  return (
    <Page title="General">
      <Group title="Look">
        <Row label="Theme">
          <Segmented
            id="theme"
            value={s.theme}
            onChange={(theme) => void update({ theme })}
            options={[
              { value: "system", label: "System" },
              { value: "light", label: "Light" },
              { value: "dark", label: "Dark" },
            ]}
          />
        </Row>
        <Row label="Text size" hint="For the conversation">
          <Stepper value={s.textSize} min={12} max={20} onChange={(textSize) => void update({ textSize })} />
        </Row>
        <Row label="Conversation width">
          <Segmented
            id="width"
            value={s.chatWidth}
            onChange={(chatWidth) => void update({ chatWidth })}
            options={[
              { value: "narrow", label: "Narrow" },
              { value: "normal", label: "Normal" },
              { value: "wide", label: "Wide" },
            ]}
          />
        </Row>
      </Group>

      <Group title="Chats">
        <Row label="Send with" hint={s.sendKey === "enter" ? "Shift+Enter starts a new line" : "Enter starts a new line"}>
          <Segmented
            id="send"
            value={s.sendKey}
            onChange={(sendKey) => void update({ sendKey })}
            options={[
              { value: "enter", label: "Enter" },
              { value: "ctrl-enter", label: "Ctrl+Enter" },
            ]}
          />
        </Row>
        <Row label="Name chats automatically" hint="A model suggests a short title for each new chat. Off: your first message is the title">
          <Toggle label="Name chats automatically" on={s.autoTitles} onChange={(autoTitles) => void update({ autoTitles })} />
        </Row>
        {s.autoTitles && (
          <Row label="Model for titles" hint="A small, fast one is plenty. On the chat's own server it runs before the reply">
            <TitleModelPicker value={s.titleModel} onPick={(titleModel) => void update({ titleModel })} />
          </Row>
        )}
        <Row label="Show reasoning while the model thinks" hint="It folds away once the answer starts; you can always open it again">
          <Toggle label="Show reasoning while the model thinks" on={s.expandThinking} onChange={(expandThinking) => void update({ expandThinking })} />
        </Row>
        <Row label="Reply stats" hint="Speed, tokens and time to first token under each reply">
          <Toggle label="Reply stats" on={s.showStats} onChange={(showStats) => void update({ showStats })} />
        </Row>
      </Group>

      <Group title="When a reply is ready">
        <Row label="Notify in Holster" hint="While Relay is in the background, Holster shows the reply, with a button that opens the chat">
          <Toggle label="Notify in Holster" on={s.notifyOnReply} onChange={(notifyOnReply) => void update({ notifyOnReply })} />
        </Row>
        <Row label="Glow in Holster" hint={s.notifyOnReply ? "Holster's tab lights up when Relay is muted there" : "Holster's tab lights up while Relay is in the background"}>
          <Toggle label="Glow in Holster" on={s.holsterGlow} onChange={(holsterGlow) => void update({ holsterGlow })} />
        </Row>
        <HolsterRow />
      </Group>

      <Group title="General">
        <Row label="Start with Windows" hint="Relay waits in the tray until you need it">
          <Toggle label="Start with Windows" on={s.launchAtStartup} onChange={(launchAtStartup) => void update({ launchAtStartup })} />
        </Row>
        <Row label="The close button" hint={s.closeAction === "tray" ? "Relay keeps running in the tray; quit from the tray menu" : "Closing the window quits Relay"}>
          <Segmented
            id="close"
            value={s.closeAction}
            onChange={(closeAction) => void update({ closeAction })}
            options={[
              { value: "tray", label: "Keeps it in the tray" },
              { value: "quit", label: "Quits" },
            ]}
          />
        </Row>
        <Row label="Data folder" hint={dataDir}>
          <Button variant="default" onClick={() => void revealItemInDir(dataDir + "\\relay.db")}>
            Open data folder
          </Button>
        </Row>
      </Group>

      <Group title="Chat history">
        <Row label="Delete all chats" hint="Every conversation, for good. Providers, presets and settings stay">
          <ConfirmButton
            label={
              <>
                <Trash2 size={14} />
                Delete all
              </>
            }
            question="Delete every chat?"
            confirm="Delete all"
            onConfirm={async () => {
              await api.deleteAllChats();
              useStore.setState({ chats: [], currentId: null, current: null, hits: null });
              useStore.getState().toast("All chats deleted");
            }}
          />
        </Row>
      </Group>
    </Page>
  );
}

function HolsterRow() {
  const [st, setSt] = useState<HolsterStatus | null>(null);
  useEffect(() => {
    void api.holsterStatus().then(setSt);
  }, []);
  if (!st) return null;
  const text = st.sideBySide
    ? "Relay Dev leaves the dock's tile to the installed Relay"
    : !st.installed
    ? "Holster isn't installed on this PC"
    : st.error
      ? st.error
      : st.registered
        ? st.warnings.length
          ? st.warnings.join(" · ")
          : "Relay has its tile in the dock"
        : "Registering…";
  return (
    <Row label="Holster" hint={text}>
      <Dot state={st.installed && st.registered && !st.error ? "ok" : st.error ? "error" : "off"} />
    </Row>
  );
}
