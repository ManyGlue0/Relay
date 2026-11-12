import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { Download, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { api, errorText, on } from "../../lib/api";
import { BrandMark } from "../TitleBar";
import { Button } from "../ui";
import { Group, Page, Row } from "./kit";

const REPO = "https://github.com/ManyGlue0/Relay";

export function About() {
  const [info, setInfo] = useState<{ version: string; dataDir: string } | null>(null);
  useEffect(() => {
    void api.appInfo().then(setInfo);
  }, []);
  return (
    <Page title="About">
      <div className="about">
        <span className="about-tile">
          <BrandMark size={34} />
        </span>
        <div>
          <div className="about-name">Relay</div>
          <div className="row-hint">Version {info?.version ?? "…"} · A chat client for your models</div>
        </div>
      </div>
      <Updates version={info?.version} />
      <Group>
        <Row label="Your data" hint={info ? `${info.dataDir} · chats stay on this PC; keys live in Windows Credential Manager` : undefined}>
          <Button variant="default" onClick={() => info && void revealItemInDir(info.dataDir + "\\relay.db")}>
            Open data folder
          </Button>
        </Row>
        <Row label="Source code" hint="Relay is open source. Ideas and bug reports are welcome there">
          <Button variant="ghost" onClick={() => void openUrl(REPO)}>
            Relay on GitHub
          </Button>
        </Row>
        <Row label="Holster" hint="Relay lives in the Holster dock, a home for your own apps">
          <Button variant="ghost" onClick={() => void openUrl("https://github.com/ManyGlue0/Holster")}>
            Holster on GitHub
          </Button>
        </Row>
        <Row label="License" hint="MIT. Built with Tauri, React, Framer Motion, Lucide, KaTeX and highlight.js">
          <span />
        </Row>
      </Group>
    </Page>
  );
}

type UpdateState =
  | { step: "idle" | "checking" | "latest" | "installing" }
  | { step: "available"; version: string }
  | { step: "downloading"; version: string; pct: number | null }
  | { step: "error"; message: string };

/** Checks GitHub for a newer Relay when asked, and installs it on Update
 *  (the same card as Holster's). */
function Updates({ version }: { version?: string }) {
  const [state, setState] = useState<UpdateState>({ step: "idle" });

  useEffect(
    () =>
      on<{ downloaded: number; total: number | null }>("update-progress", ({ downloaded, total }) =>
        setState((s) => (s.step === "downloading" ? { ...s, pct: total ? Math.min(100, Math.round((downloaded / total) * 100)) : null } : s)),
      ),
    [],
  );

  const check = async () => {
    setState({ step: "checking" });
    try {
      const found = await api.updateCheck();
      setState(found ? { step: "available", version: found.version } : { step: "latest" });
    } catch (e) {
      setState({ step: "error", message: errorText(e) });
    }
  };
  const install = async (version: string) => {
    setState({ step: "downloading", version, pct: 0 });
    try {
      await api.updateInstall();
      setState({ step: "installing" });
    } catch (e) {
      setState({ step: "error", message: errorText(e) });
    }
  };

  const hint =
    state.step === "checking"
      ? "Checking…"
      : state.step === "latest"
        ? "You have the latest version"
        : state.step === "available"
          ? `Version ${state.version} is ready. Relay restarts by itself`
          : state.step === "downloading"
            ? `Downloading ${state.version}${state.pct === null ? "…" : `, ${state.pct}%`}`
            : state.step === "installing"
              ? "Installing. Relay restarts by itself"
              : state.step === "error"
                ? state.message
                : "Looks for a newer version on GitHub";

  return (
    <Group title="Updates">
      <Row label={version ? `Relay ${version}` : "Relay"} hint={hint} className={state.step === "error" ? "row-warn" : ""}>
        {state.step === "available" ? (
          <Button variant="primary" onClick={() => void install(state.version)}>
            <Download size={14} /> Update
          </Button>
        ) : (
          <Button variant="ghost" disabled={state.step === "checking" || state.step === "downloading" || state.step === "installing"} onClick={() => void check()}>
            <RefreshCw size={14} className={state.step === "checking" ? "spin" : ""} /> Check
          </Button>
        )}
      </Row>
    </Group>
  );
}
