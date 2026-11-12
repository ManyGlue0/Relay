import { AnimatePresence, motion } from "framer-motion";
import { AudioLines, ExternalLink, FileText, Film, Loader, Mic, Pause, Play, Presentation, TriangleAlert, X, type LucideIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { fileUrl, type Ability, type Attachment, type Caps } from "../lib/api";
import { clock, fileSize } from "../lib/media";
import { spring } from "../lib/motion";
import { closePdf, drawPage, openPdf, pageSize } from "../lib/pdf";
import { shortModel, useActiveModel, useEscape, useStore, type DraftFile } from "../lib/store";
import { highlight } from "./Markdown";
import { IconButton, Popover, Toggle, type Anchor } from "./ui";

const KIND_LABEL: Record<string, string> = { pdf: "PDF", document: "Document", text: "Text", audio: "Audio", video: "Video", image: "Image" };
/** PDFs longer than this get a warning when their pages go as images. */
const LONG_PDF = 50;

function kindIcon(a: Attachment): LucideIcon {
  if (a.voice) return Mic;
  if (a.kind === "audio") return AudioLines;
  if (a.kind === "video") return Film;
  if (/\.pptx$/i.test(a.name)) return Presentation;
  return FileText;
}

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

/** "PDF · 12 pages", "MD · 46 B", "0:12" */
function detail(a: Attachment): string {
  if (a.voice) return clock(a.duration);
  const ext = a.kind === "text" || a.kind === "document" ? a.name.split(".").pop()?.toUpperCase() : KIND_LABEL[a.kind];
  if (a.kind === "pdf") {
    if (a.pages) return `PDF · ${plural(a.pages, "page")} as images`;
    if (a.pageCount) return `PDF · ${plural(a.pageCount, "page")}`;
  }
  const parts = [ext ?? KIND_LABEL[a.kind], a.duration ? clock(a.duration) : fileSize(a.size)];
  if (a.kind === "document" && a.chars === 0) parts.push("no text found");
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
  // A PDF without text goes as its pages, unless the server reads PDFs itself.
  if (a.kind === "pdf" && a.pages && !caps.pdf && !caps.vision) return "vision";
  return null;
}

/** What to warn about on a tile: an ability the model lacks, or a long PDF going as images. */
type Note = { title: string; body: string; ability?: Ability };

function noteFor(a: Attachment, caps: Caps | undefined, model: string): Note | null {
  const missing = missingAbility(a, caps);
  if (missing) {
    return { title: `${model} ${ABILITY_WORDS[missing].may}`, body: "Relay sends it anyway. If the model can't take it, the server will say so.", ability: missing };
  }
  if (a.kind === "pdf" && a.pages > LONG_PDF && !caps?.pdf) {
    return {
      title: `${a.pages} pages go as images`,
      body: "This PDF has no text, so the model gets every page as a picture. That much can fill a model's context, and slow it down.",
    };
  }
  return null;
}

// ---- in the composer -----------------------------------------------------------------

export function AttachmentTray({ files }: { files: DraftFile[] }) {
  const model = useActiveModel();
  const caps = useStore((s) => (model ? s.caps[`${model.providerId}/${model.model}`]?.caps : undefined));
  const [shown, setShown] = useState<Attachment | null>(null);
  useEffect(() => {
    if (model && files.length) useStore.getState().ensureCaps(model);
  }, [model, files.length]);
  const name = model ? shortModel(model.model) : "This model";
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
            <DraftTile file={f} note={f.attachment && f.ready ? noteFor(f.attachment, caps, name) : null} onOpen={() => f.attachment && setShown(f.attachment)} />
          </motion.div>
        ))}
      </AnimatePresence>
      <Preview file={shown} onClose={() => setShown(null)} />
    </div>
  );
}

function DraftTile({ file, note, onOpen }: { file: DraftFile; note: Note | null; onOpen: () => void }) {
  const a = file.attachment;
  const remove = (
    <button
      className="att-remove"
      aria-label={`Remove ${file.name}`}
      title="Remove"
      onClick={(e) => {
        e.stopPropagation();
        useStore.getState().detach(file.key);
      }}
    >
      <X size={12} strokeWidth={2.4} />
    </button>
  );
  const busy = !file.ready && (
    <span className="att-busy" aria-label="Preparing">
      <Loader size={15} className="spin" />
    </span>
  );
  const warn = note && <NoteBadge note={note} />;
  const open = {
    role: "button",
    tabIndex: a ? 0 : -1,
    onClick: () => a && onOpen(),
    onKeyDown: (e: React.KeyboardEvent) => {
      if (a && (e.key === "Enter" || e.key === " ")) {
        e.preventDefault();
        onOpen();
      }
    },
  };
  if (a && (a.kind === "image" || (a.kind === "video" && a.frames?.length))) {
    const src = a.kind === "image" ? fileUrl(a, a.thumb ? "x/thumb.png" : a.file) : fileUrl(a, "x/frame-0.jpg");
    return (
      <div className={`att att-thumb ${note ? "warn" : ""}`} title={`${a.name} (click to see it)`} {...open}>
        <img src={src} alt={a.name} draggable={false} />
        {a.kind === "video" && <span className="att-badge">{clock(a.duration)}</span>}
        {busy}
        {warn}
        {remove}
      </div>
    );
  }
  const Icon = a ? kindIcon(a) : FileText;
  return (
    <div className={`att att-file ${note ? "warn" : ""}`} title={a ? `${file.name} (click to see it)` : file.name} {...open}>
      <span className="att-icon">{busy || <Icon size={17} />}</span>
      <span className="att-text">
        <span className="att-name">{a?.voice ? "Voice message" : file.name}</span>
        <span className="att-sub">{!file.ready && file.progress ? file.progress : a ? detail(a) : "Adding…"}</span>
      </span>
      {warn}
      {remove}
    </div>
  );
}

/** A warning on a tile; a click explains it (and, for a missing ability, offers "It can"). */
function NoteBadge({ note }: { note: Note }) {
  const model = useActiveModel();
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  return (
    <>
      <button
        ref={btn}
        className="att-warn"
        aria-label={note.title}
        title={note.title}
        onClick={(e) => {
          e.stopPropagation();
          const r = btn.current!.getBoundingClientRect();
          setAnchor(anchor ? null : { x: Math.round(r.left), y: Math.round(r.top - 6), above: true });
        }}
      >
        <TriangleAlert size={12} strokeWidth={2.4} />
      </button>
      <Popover anchor={anchor} onClose={() => setAnchor(null)} width={280} trigger={btn}>
        <div className="menu caps-note" onClick={(e) => e.stopPropagation()}>
          <div className="caps-note-title">{note.title}</div>
          <div className="menu-note">{note.body}</div>
          {model && note.ability && (
            <div className="menu-toggle-row">
              <span>{ABILITY_WORDS[note.ability].can}</span>
              <Toggle
                label={ABILITY_WORDS[note.ability].can}
                on={false}
                onChange={(on) => {
                  setAnchor(null);
                  void useStore.getState().setCap(model, note.ability!, on);
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
                <img src={fileUrl(a, !single && a.thumb ? "x/thumb.png" : a.file)} alt={a.name} draggable={false} />
              </button>
              {onRemove && <RemoveButton name={a.name} onClick={() => onRemove(a.id)} />}
            </div>
          ))}
        </div>
      )}
      {others.map((a) => (
        <div key={a.id} className="msg-file-wrap">
          {a.kind === "audio" ? <AudioPlayer a={a} /> : a.kind === "video" ? <VideoPlayer a={a} /> : <FileCard a={a} onOpen={() => setShown(a)} />}
          {onRemove && <RemoveButton name={a.name} onClick={() => onRemove(a.id)} />}
        </div>
      ))}
      <Preview file={shown} onClose={() => setShown(null)} />
    </div>
  );
}

function RemoveButton({ name, onClick }: { name: string; onClick: () => void }) {
  return (
    <button className="att-remove shown" aria-label={`Remove ${name}`} title="Remove" onClick={onClick}>
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

function FileCard({ a, onOpen }: { a: Attachment; onOpen: () => void }) {
  const Icon = kindIcon(a);
  return (
    <button className="att att-file msg-file" title={`See ${a.name}`} onClick={onOpen}>
      <span className="att-icon">
        <Icon size={17} />
      </span>
      <span className="att-text">
        <span className="att-name">{a.name}</span>
        <span className="att-sub">{detail(a)}</span>
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

// ---- the preview ---------------------------------------------------------------------------

/**
 * A file, big, over the app: an image or video on its own, audio with its
 * player, and text, documents and PDFs on a sheet (PDFs page by page).
 * Escape or a click outside closes it.
 */
export function Preview({ file, onClose }: { file: Attachment | null; onClose: () => void }) {
  useEscape(!!file, onClose);
  const media = file && (file.kind === "image" || file.kind === "video");
  return createPortal(
    <AnimatePresence>
      {file && (
        <motion.div
          className="preview"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0, transition: { duration: 0.14 } }}
          onClick={onClose}
          onContextMenu={(e) => e.preventDefault()}
        >
          {media ? (
            <>
              <motion.div className="preview-media" initial={{ scale: 0.96, opacity: 0 }} animate={{ scale: 1, opacity: 1 }} exit={{ scale: 0.98, opacity: 0 }} transition={spring}>
                {file.kind === "image" ? (
                  <img src={fileUrl(file)} alt={file.name} draggable={false} onClick={(e) => e.stopPropagation()} />
                ) : (
                  <video src={fileUrl(file)} controls autoPlay onClick={(e) => e.stopPropagation()} />
                )}
              </motion.div>
              <div className="preview-bar" onClick={(e) => e.stopPropagation()}>
                <span className="preview-bar-name">{file.name}</span>
                <span className="preview-bar-sub">{file.width && file.height ? `${file.width} × ${file.height}` : detail(file)}</span>
                <button className="preview-bar-close" onClick={onClose}>
                  Close
                </button>
              </div>
            </>
          ) : (
            <motion.div
              className={`preview-sheet ${file.kind === "audio" ? "small" : file.kind === "pdf" ? "tall" : ""}`}
              initial={{ opacity: 0, y: 14, scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: 8, scale: 0.985, transition: { duration: 0.14 } }}
              transition={spring}
              onClick={(e) => e.stopPropagation()}
            >
              <SheetHead file={file} onClose={onClose} />
              <div className="preview-body">
                {file.kind === "audio" ? <AudioPlayer a={file} /> : file.kind === "pdf" ? <PdfPages file={file} /> : <FileText_ file={file} />}
              </div>
            </motion.div>
          )}
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}

function SheetHead({ file, onClose }: { file: Attachment; onClose: () => void }) {
  const Icon = kindIcon(file);
  return (
    <div className="preview-head">
      <span className="att-icon">
        <Icon size={17} />
      </span>
      <span className="att-text">
        <span className="att-name">{file.voice ? "Voice message" : file.name}</span>
        <span className="att-sub">{detail(file)}</span>
      </span>
      <div className="preview-fill" />
      <IconButton label="Open in its app" size="small" onClick={() => void useStore.getState().openAttachment(file.id)}>
        <ExternalLink size={15} />
      </IconButton>
      <IconButton label="Close (Esc)" size="small" onClick={onClose}>
        <X size={16} />
      </IconButton>
    </div>
  );
}

/** Shown at most, so a huge log doesn't freeze the window. */
const PREVIEW_CHARS = 200_000;
const CODE: Record<string, string> = {
  rs: "rust", ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript", py: "python",
  json: "json", jsonc: "json", md: "markdown", html: "xml", htm: "xml", xml: "xml", svg: "xml", css: "css", scss: "scss", yaml: "yaml",
  yml: "yaml", toml: "ini", ini: "ini", sh: "bash", bash: "bash", ps1: "powershell", c: "c", h: "c", cpp: "cpp", hpp: "cpp", cc: "cpp",
  cs: "csharp", java: "java", kt: "kotlin", go: "go", sql: "sql", rb: "ruby", php: "php", lua: "lua", swift: "swift", diff: "diff",
};

/** A text file (with colors for code) or a document's text. */
function FileText_({ file }: { file: Attachment }) {
  const [text, setText] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    const src = file.kind === "text" ? fileUrl(file) : fileUrl(file, "x/text.txt");
    fetch(src)
      .then((r) => (r.ok ? r.text() : Promise.reject(new Error(String(r.status)))))
      .then((t) => alive && setText(t))
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [file]);
  const lang = file.kind === "text" ? CODE[file.name.split(".").pop()?.toLowerCase() ?? ""] : undefined;
  const cut = text !== null && text.length > PREVIEW_CHARS;
  const shown = text === null ? "" : cut ? text.slice(0, PREVIEW_CHARS) : text;
  const body = useMemo(() => (lang && shown ? highlight(shown, lang) : shown), [lang, shown]);
  if (failed) return <div className="preview-empty">The file couldn't be read.</div>;
  if (text === null) return <PreviewLoading />;
  if (!text.trim()) return <div className="preview-empty">{file.kind === "text" ? "The file is empty." : "No text could be read from this file."}</div>;
  return (
    <>
      <pre className={`preview-text ${file.kind === "text" ? "mono" : "prose"}`}>
        <code className={lang ? "hljs" : undefined}>{body}</code>
      </pre>
      {cut && <div className="preview-more">Showing the first {PREVIEW_CHARS.toLocaleString()} characters of {text.length.toLocaleString()}.</div>}
    </>
  );
}

function PreviewLoading() {
  return (
    <div className="preview-empty">
      <Loader size={16} className="spin" />
    </div>
  );
}

/** A PDF, page by page; pages draw as they scroll into view. */
function PdfPages({ file }: { file: Attachment }) {
  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    let opened: PDFDocumentProxy | null = null;
    openPdf(fileUrl(file))
      .then((d) => {
        opened = d;
        if (alive) setDoc(d);
        else closePdf(d);
      })
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
      if (opened) closePdf(opened);
    };
  }, [file]);
  // A PDF the viewer can't open still shows its text.
  if (failed) return <FileText_ file={file} />;
  if (!doc) return <PreviewLoading />;
  return (
    <div className="pdf-pages">
      {Array.from({ length: doc.numPages }, (_, i) => (
        <PdfPage key={i} doc={doc} n={i + 1} />
      ))}
    </div>
  );
}

function PdfPage({ doc, n }: { doc: PDFDocumentProxy; n: number }) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [ratio, setRatio] = useState(1.414);
  const [drawn, setDrawn] = useState(false);
  const started = useRef(false);
  useEffect(() => {
    void pageSize(doc, n).then((s) => setRatio(s.height / s.width)).catch(() => {});
  }, [doc, n]);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const io = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting || !canvas.current || started.current) return;
        io.disconnect();
        started.current = true;
        const w = el.clientWidth * window.devicePixelRatio;
        void drawPage(doc, n, Math.round(w * Math.max(1, ratio)), canvas.current)
          .then(() => setDrawn(true))
          .catch(() => {});
      },
      { rootMargin: "800px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [doc, n, ratio]);
  return (
    <div ref={box} className={`pdf-page ${drawn ? "drawn" : ""}`} style={{ aspectRatio: `1 / ${ratio}` }}>
      <canvas ref={canvas} />
      <span className="pdf-page-num">{n}</span>
    </div>
  );
}
