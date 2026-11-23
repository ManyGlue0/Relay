import { AnimatePresence, motion } from "framer-motion";
import { ChevronRight, ExternalLink, FileMinus2, FilePen, FilePlus2, GitCompareArrows, RefreshCw, RotateCcw, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type Changes, type Diff, type FileVersion } from "../lib/api";
import { spring } from "../lib/motion";
import { useStore } from "../lib/store";
import { fileSize } from "../lib/media";
import { sentAgo, useNow } from "../lib/time";
import { DiffView } from "./AgentCards";
import { IconButton } from "./ui";

/** The session's files: what it created, changed or deleted in its folder,
 *  each with its versions from the checkpoints (diff against now, restore). */
export function FilesPanel() {
  const open = useStore((s) => s.filesPanel);
  const chat = useStore((s) => (s.current && s.current.id === s.currentId && s.current.kind === "agent" ? s.current : null));
  const streaming = useStore((s) => (chat ? !!s.streaming[chat.id] : false));
  const [changes, setChanges] = useState<Changes | null>(null);
  const [busy, setBusy] = useState(false);
  const chatId = chat?.id;

  const load = useCallback(async () => {
    if (!chatId) return;
    setBusy(true);
    try {
      setChanges(await api.sessionChanges(chatId));
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setBusy(false);
    }
  }, [chatId]);

  // Fresh when opened, when the session changes, and when a reply ends.
  useEffect(() => {
    if (open && chatId && !streaming) void load();
  }, [open, chatId, streaming, load]);

  return (
    <AnimatePresence initial={false}>
      {open && chat && (
        <motion.aside
          className="files-panel"
          initial={{ width: 0, opacity: 0 }}
          animate={{ width: 340, opacity: 1 }}
          exit={{ width: 0, opacity: 0 }}
          transition={spring}
        >
          <div className="files-inner">
            <div className="files-head">
              <span className="files-title">Files</span>
              <span className="files-fill" />
              <IconButton size="small" label="Refresh" onClick={() => void load()} disabled={busy}>
                <RefreshCw size={14} className={busy ? "spin" : ""} />
              </IconButton>
              <IconButton size="small" label="Close" onClick={() => useStore.setState({ filesPanel: false })}>
                <X size={15} />
              </IconButton>
            </div>
            <div className="files-body">
              {changes && changes.files.length === 0 && (
                <div className="files-empty">{changes.none ? "Nothing changed yet. Files the session creates, changes or deletes show up here." : "Everything is as it was when the session started."}</div>
              )}
              {changes?.files.map((f) => (
                <FileRow key={f.path} chatId={chat.id} path={f.path} kind={f.kind} versions={f.versions} onChanged={load} locked={streaming} />
              ))}
              {changes && changes.big.length > 0 && (
                <div className="files-big">
                  <div className="files-big-head">Not protected (too big for checkpoints)</div>
                  {changes.big.map(([p, size]) => (
                    <div key={p} className="files-big-row">
                      <span className="files-path">{p}</span>
                      <span className="files-size">{fileSize(size)}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        </motion.aside>
      )}
    </AnimatePresence>
  );
}

const KIND = {
  created: { icon: FilePlus2, label: "Created" },
  changed: { icon: FilePen, label: "Changed" },
  deleted: { icon: FileMinus2, label: "Deleted" },
} as const;

function FileRow({ chatId, path, kind, versions, onChanged, locked }: { chatId: string; path: string; kind: "created" | "changed" | "deleted"; versions: number; onChanged: () => void; locked: boolean }) {
  const [open, setOpen] = useState(false);
  const [list, setList] = useState<FileVersion[] | null>(null);
  const [diff, setDiff] = useState<{ hash: string; diff: Diff } | null>(null);
  const now = useNow();
  const K = KIND[kind];
  useEffect(() => {
    if (!open) return;
    void api
      .fileVersions(chatId, path)
      .then((v) => setList(v.slice().reverse()))
      .catch((e) => useStore.getState().toast(errorText(e)));
  }, [open, chatId, path, versions]);
  const name = path.split("/").pop() ?? path;
  const dir = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
  return (
    <div className={`files-row ${kind}`}>
      <button className="files-row-head" onClick={() => setOpen(!open)} title={`${K.label}: ${path}`}>
        <K.icon size={14} className="files-kind" />
        <span className="files-name">{name}</span>
        {dir && <span className="files-dir">{dir}</span>}
        <span className="files-fill" />
        <motion.span className="pill-chev" animate={{ rotate: open ? 90 : 0 }} transition={spring}>
          <ChevronRight size={13} />
        </motion.span>
      </button>
      {open && (
        <div className="files-versions">
          {kind !== "deleted" && (
            <button className="files-action" onClick={() => void api.openSessionFile(chatId, path).catch((e) => useStore.getState().toast(errorText(e)))}>
              <ExternalLink size={13} />
              Open
            </button>
          )}
          {list?.length === 0 && <div className="files-none">No saved versions.</div>}
          {list?.map((v, i) => (
            <div key={v.hash} className="files-version">
              <div className="files-version-row">
                <span className="files-when">{i === 0 && kind !== "deleted" ? "Latest saved" : sentAgo(v.at, now)}</span>
                <span className="files-size">{fileSize(v.size)}</span>
                <span className="files-fill" />
                <IconButton
                  size="tiny"
                  label={diff?.hash === v.hash ? "Hide the changes" : "Changes since this version"}
                  active={diff?.hash === v.hash}
                  onClick={async () => {
                    if (diff?.hash === v.hash) return setDiff(null);
                    try {
                      setDiff({ hash: v.hash, diff: await api.versionDiff(chatId, path, v.hash) });
                    } catch (e) {
                      useStore.getState().toast(errorText(e));
                    }
                  }}
                >
                  <GitCompareArrows size={13} />
                </IconButton>
                <IconButton
                  size="tiny"
                  label={locked ? "Wait for the reply to finish" : "Restore this version"}
                  disabled={locked}
                  onClick={async () => {
                    try {
                      const undo = await api.restoreFile(chatId, path, v.hash);
                      useStore.getState().toast(`Restored ${name}`, { label: "Undo", run: () => void undoRestore(chatId, undo, onChanged) });
                      onChanged();
                    } catch (e) {
                      useStore.getState().toast(errorText(e));
                    }
                  }}
                >
                  <RotateCcw size={13} />
                </IconButton>
              </div>
              {diff?.hash === v.hash &&
                (diff.diff.hunks.length ? <DiffView rows={diff.diff.hunks} truncated={diff.diff.truncated} /> : <div className="files-none">Same as now.</div>)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** Undoes a restore: back to the checkpoint taken right before it. */
export async function undoRestore(chatId: string, checkpoint: number, after?: () => void) {
  try {
    await api.restore(chatId, checkpoint);
    useStore.getState().toast("Undone");
    after?.();
  } catch (e) {
    useStore.getState().toast(errorText(e));
  }
}
