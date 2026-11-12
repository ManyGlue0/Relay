import { AnimatePresence, motion } from "framer-motion";
import { AudioLines, FileText, Film, Loader, Mic, Pause, Play, Presentation, TriangleAlert, X, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { fileUrl, type Ability, type Attachment, type Caps } from "../lib/api";
import { clock, fileSize } from "../lib/media";
import { spring } from "../lib/motion";
import { shortModel, useActiveModel, useEscape, useStore, type DraftFile } from "../lib/store";
import { Popover, Toggle, type Anchor } from "./ui";

const KIND_LABEL: Record<string, string> = { pdf: "PDF", document: "Document", text: "Text", audio: "Audio", video: "Video", image: "Image" };

function kindIcon(a: Attachment): LucideIcon {
  if (a.voice) return Mic;
  if (a.kind === "audio") return AudioLines;
  if (a.kind === "video") return Film;
  if (/\.pptx$/i.test(a.name)) return Presentation;
  return FileText;
}

/** "PDF · 2.4 MB", "Voice message · 0:12" */
function detail(a: Attachment): string {
  if (a.voice) return clock(a.duration);
  const ext = a.kind === "text" || a.kind === "document" ? a.name.split(".").pop()?.toUpperCase() : KIND_LABEL[a.kind];
  const parts = [ext ?? KIND_LABEL[a.kind], a.duration ? clock(a.duration) : fileSize(a.size)];
  if ((a.kind === "pdf" || a.kind === "document") && a.chars === 0) parts.push("no text found");
  return parts.filter(Boolean).join(" · ");
}

const ABILITY_WORDS: Record<Ability, { may: string; can: string }> = {
  vision: { may: "may not see images", can: "It can see images" },
  audio: { may: "may not hear audio", can: "It can hear audio" },
  video: { may: "may not watch video", can: "It can watch video" },
  pdf: { may: "may not read PDFs", can: "It reads PDFs as files" },
};

/** The ability a file needs that the model doesn't seem to have (null: fine, or unknown). */
export function missingAbility(a: Attachment, caps: Caps | undefined): Ability | null {
  if (!caps) return null;
  if (a.kind === "image" && !caps.vision) return "vision";
  if (a.kind === "audio" && !caps.audio) return "audio";
  // Without video, a video goes as stills: fine for a model that sees.
  if (a.kind === "video" && !caps.video && !caps.vision) return "vision";
  return null;
}

// ---- in the composer -----------------------------------------------------------------

export function AttachmentTray({ files }: { files: DraftFile[] }) {
  const model = useActiveModel();
  const caps = useStore((s) => (model ? s.caps[`${model.providerId}/${model.model}`]?.caps : undefined));
  useEffect(() => {
    if (model && files.length) useStore.getState().ensureCaps(model);
  }, [model, files.length]);
  return (
    <div className="tray">
      <AnimatePresence initial={false}>
        {files.map((f) => (
          <motion.div
            key={f.key}
            layout="position"
            className="tray-item"
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.9, transition: { duration: 0.12 } }}
            transition={spring}
          >
            <DraftTile file={f} missing={f.attachment ? missingAbility(f.attachment, caps) : null} />
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}

function DraftTile({ file, missing }: { file: DraftFile; missing: Ability | null }) {
  const a = file.attachment;
  const remove = (
    <button className="tile-remove" aria-label={`Remove ${file.name}`} title="Remove" onClick={() => useStore.getState().detach(file.key)}>
      <X size={12} strokeWidth={2.4} />
    </button>
  );
  const busy = !file.ready && (
    <span className="tile-busy" aria-label="Preparing">
      <Loader size={15} className="spin" />
    </span>
  );
  const warn = missing && <MissingBadge ability={missing} />;
  if (a && (a.kind === "image" || (a.kind === "video" && a.frames?.length))) {
    return (
      <div className={`tile tile-thumb ${missing ? "warn" : ""}`} title={a.name}>
        <img src={fileUrl(a, a.kind === "image" ? a.file : "x/frame-0.jpg")} alt={a.name} draggable={false} />
        {a.kind === "video" && <span className="tile-badge">{clock(a.duration)}</span>}
        {busy}
        {warn}
        {remove}
      </div>
    );
  }
  const Icon = a ? kindIcon(a) : FileText;
  return (
    <div className={`tile tile-file ${missing ? "warn" : ""}`} title={file.name}>
      <span className="tile-icon">{busy || <Icon size={17} />}</span>
      <span className="tile-text">
        <span className="tile-name">{a?.voice ? "Voice message" : file.name}</span>
        <span className="tile-sub">{a ? detail(a) : "Adding…"}</span>
      </span>
      {warn}
      {remove}
    </div>
  );
}

/** "This model may not hear audio", with a way to say it can. */
function MissingBadge({ ability }: { ability: Ability }) {
  const model = useActiveModel();
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const words = ABILITY_WORDS[ability];
  const name = model ? shortModel(model.model) : "This model";
  return (
    <>
      <button
        ref={btn}
        className="tile-warn"
        aria-label={`${name} ${words.may}`}
        title={`${name} ${words.may}`}
        onClick={() => {
          const r = btn.current!.getBoundingClientRect();
          setAnchor(anchor ? null : { x: Math.round(r.left), y: Math.round(r.top - 6), above: true });
        }}
      >
        <TriangleAlert size={11} strokeWidth={2.4} />
      </button>
      <Popover anchor={anchor} onClose={() => setAnchor(null)} width={280} trigger={btn}>
        <div className="menu caps-note">
          <div className="caps-note-title">
            {name} {words.may}
          </div>
          <div className="menu-note">Relay sends it anyway. If the model can't take it, the server will say so.</div>
          {model && (
            <div className="menu-toggle-row">
              <span>{words.can}</span>
              <Toggle
                label={words.can}
                on={false}
                onChange={(on) => {
                  setAnchor(null);
                  void useStore.getState().setCap(model, ability, on);
                }}
              />
            </div>
          )}
        </div>
      </Popover>
    </>
  );
}

// ---- in a message ----------------------------------------------------------------------

export function MessageFiles({ files, onRemove }: { files: Attachment[]; onRemove?: (id: string) => void }) {
  const [shown, setShown] = useState<Attachment | null>(null);
  const images = files.filter((a) => a.kind === "image");
  const others = files.filter((a) => a.kind !== "image");
  const single = images.length === 1;
  return (
    <div className="msg-files">
      {images.length > 0 && (
        <div className={`msg-images ${single ? "single" : ""}`}>
          {images.map((a) => (
            <div key={a.id} className="msg-image-wrap">
              <button className="msg-image" onClick={() => setShown(a)} title={a.name} style={single ? fitted(a) : undefined}>
                <img src={fileUrl(a)} alt={a.name} draggable={false} />
              </button>
              {onRemove && <RemoveButton name={a.name} onClick={() => onRemove(a.id)} />}
            </div>
          ))}
        </div>
      )}
      {others.map((a) => (
        <div key={a.id} className="msg-file-wrap">
          {a.kind === "audio" ? <AudioPlayer a={a} /> : a.kind === "video" ? <VideoPlayer a={a} /> : <FileCard a={a} />}
          {onRemove && <RemoveButton name={a.name} onClick={() => onRemove(a.id)} />}
        </div>
      ))}
      <Lightbox image={shown} onClose={() => setShown(null)} />
    </div>
  );
}

function RemoveButton({ name, onClick }: { name: string; onClick: () => void }) {
  return (
    <button className="tile-remove shown" aria-label={`Remove ${name}`} title="Remove" onClick={onClick}>
      <X size={12} strokeWidth={2.4} />
    </button>
  );
}

/** A lone image keeps its shape, within 320 × 320. */
function fitted(a: Attachment): React.CSSProperties | undefined {
  if (!a.width || !a.height) return undefined;
  const scale = Math.min(1, 320 / a.width, 320 / a.height);
  return { width: Math.max(48, Math.round(a.width * scale)), height: Math.max(48, Math.round(a.height * scale)) };
}

function FileCard({ a }: { a: Attachment }) {
  const Icon = kindIcon(a);
  return (
    <button className="tile tile-file msg-file" title={`Open ${a.name}`} onClick={() => void useStore.getState().openAttachment(a.id)}>
      <span className="tile-icon">
        <Icon size={17} />
      </span>
      <span className="tile-text">
        <span className="tile-name">{a.name}</span>
        <span className="tile-sub">{detail(a)}</span>
      </span>
    </button>
  );
}

function VideoPlayer({ a }: { a: Attachment }) {
  const ratio = a.width && a.height ? a.width / a.height : 16 / 9;
  const width = Math.round(Math.min(360, 240 * ratio));
  return (
    <video
      className="msg-video"
      src={fileUrl(a)}
      poster={a.frames?.length ? fileUrl(a, "x/frame-0.jpg") : undefined}
      controls
      preload="metadata"
      style={{ width, height: Math.round(width / ratio) }}
    />
  );
}

export function AudioPlayer({ a }: { a: Attachment }) {
  const ref = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [t, setT] = useState(0);
  const [length, setLength] = useState(a.duration ?? 0);
  const pct = length > 0 ? Math.min(1, t / length) : 0;
  const toggle = () => {
    const el = ref.current;
    if (!el) return;
    if (el.paused) void el.play();
    else el.pause();
  };
  const seek = (e: React.PointerEvent<HTMLDivElement>) => {
    const el = ref.current;
    if (!el || !length) return;
    const r = e.currentTarget.getBoundingClientRect();
    el.currentTime = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) * length;
  };
  return (
    <div className={`audio ${a.voice ? "voice" : ""}`} title={a.voice ? "Voice message" : a.name}>
      <button className="audio-play" aria-label={playing ? "Pause" : "Play"} onClick={toggle}>
        {playing ? <Pause size={14} fill="currentColor" /> : <Play size={14} fill="currentColor" className="nudge" />}
      </button>
      <div className="audio-body">
        {!a.voice && <span className="audio-name">{a.name}</span>}
        <div className="audio-track" onPointerDown={seek}>
          <div className="audio-fill" style={{ width: `${pct * 100}%` }} />
        </div>
      </div>
      <span className="audio-time">{clock(playing || t > 0 ? t : length)}</span>
      <audio
        ref={ref}
        src={fileUrl(a)}
        preload="metadata"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => {
          setPlaying(false);
          setT(0);
        }}
        onTimeUpdate={(e) => setT(e.currentTarget.currentTime)}
        onLoadedMetadata={(e) => isFinite(e.currentTarget.duration) && setLength(e.currentTarget.duration)}
      />
    </div>
  );
}

function Lightbox({ image, onClose }: { image: Attachment | null; onClose: () => void }) {
  useEscape(!!image, onClose);
  return createPortal(
    <AnimatePresence>
      {image && (
        <motion.div
          className="lightbox"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0, transition: { duration: 0.14 } }}
          onClick={onClose}
        >
          <motion.img
            src={fileUrl(image)}
            alt={image.name}
            initial={{ scale: 0.96, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            exit={{ scale: 0.98, opacity: 0 }}
            transition={spring}
            draggable={false}
          />
          <div className="lightbox-bar" onClick={(e) => e.stopPropagation()}>
            <span className="lightbox-name">{image.name}</span>
            <button className="lightbox-open" onClick={() => void useStore.getState().openAttachment(image.id)}>
              Open
            </button>
          </div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
