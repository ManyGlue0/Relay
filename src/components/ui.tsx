import { AnimatePresence, motion } from "framer-motion";
import { Check, ChevronDown, Minus, Plus } from "lucide-react";
import { forwardRef, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { spring } from "../lib/motion";
import { useEscape, useStore } from "../lib/store";

// ---- buttons ----------------------------------------------------------------------

type IconButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & { label: string; active?: boolean; size?: "small" | "tiny" };

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { label, className, active, size, children, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      aria-label={label}
      title={label}
      className={`icon-btn ${size ?? ""} ${active ? "active" : ""} ${className ?? ""}`}
      {...rest}
    >
      {children}
    </button>
  );
});

export function Button({
  variant = "default",
  className,
  ...rest
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "ghost" | "danger" }) {
  return <button className={`btn btn-${variant} ${className ?? ""}`} {...rest} />;
}

// ---- popover ------------------------------------------------------------------------

export interface Anchor {
  x: number;
  y: number;
  /** Align the popover's right edge to x instead of its left edge. */
  alignRight?: boolean;
  /** Open upward from y (for things near the bottom, like the composer). */
  above?: boolean;
}

export function anchorFrom(el: Element, opts: { alignRight?: boolean; above?: boolean } = {}): Anchor {
  const r = el.getBoundingClientRect();
  return { x: Math.round(opts.alignRight ? r.right : r.left), y: Math.round(opts.above ? r.top - 6 : r.bottom + 6), ...opts };
}

export function Popover({
  anchor,
  onClose,
  children,
  width,
  className,
  trigger,
}: {
  anchor: Anchor | null;
  onClose: () => void;
  children: React.ReactNode;
  width?: number;
  className?: string;
  /** The button that opens it: a press there toggles instead of closing and reopening. */
  trigger?: React.RefObject<Element | null>;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const close = useCallback(() => onClose(), [onClose]);
  useEscape(anchor !== null, close);

  useLayoutEffect(() => {
    if (!anchor || !ref.current) {
      setPos(null);
      return;
    }
    const el = ref.current;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let left = anchor.alignRight ? anchor.x - w : anchor.x;
    let top = anchor.above ? anchor.y - h : anchor.y;
    left = Math.max(8, Math.min(left, window.innerWidth - w - 8));
    if (!anchor.above && top + h > window.innerHeight - 8) top = Math.max(8, anchor.y - h - 12);
    if (anchor.above && top < 8) top = 8;
    // Whole pixels: a fractional position makes the content snap when the
    // scale animation hands back to a plain layout.
    left = Math.round(left);
    top = Math.round(top);
    setPos((p) => (p && p.left === left && p.top === top ? p : { left, top }));
  }, [anchor, children]);

  useEffect(() => {
    if (!anchor) return;
    const down = (e: PointerEvent) => {
      if (!ref.current || ref.current.contains(e.target as Node)) return;
      if (trigger?.current?.contains(e.target as Node)) return; // the trigger's click toggles
      const other = (e.target as Element).closest?.(".popover");
      if (other && ref.current.compareDocumentPosition(other) & Node.DOCUMENT_POSITION_FOLLOWING) return;
      onClose();
    };
    const blur = () => onClose();
    window.addEventListener("pointerdown", down, true);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("pointerdown", down, true);
      window.removeEventListener("blur", blur);
    };
  }, [anchor, onClose, trigger]);

  // The origin is known from the anchor, so the scale never jumps mid-animation.
  const origin = anchor ? `${anchor.above ? "bottom" : "top"} ${anchor.alignRight ? "right" : "left"}` : undefined;

  return createPortal(
    <AnimatePresence>
      {anchor && (
        <motion.div
          ref={ref}
          className={`popover ${className ?? ""}`}
          style={{
            left: pos?.left ?? anchor.x,
            top: pos?.top ?? anchor.y,
            width,
            visibility: pos ? "visible" : "hidden",
            transformOrigin: origin,
          }}
          initial={{ opacity: 0, scale: 0.96, y: anchor.above ? 4 : -4 }}
          animate={{ opacity: 1, scale: 1, y: 0 }}
          exit={{ opacity: 0, scale: 0.97, y: anchor.above ? 2 : -2, transition: { duration: 0.12 } }}
          transition={spring}
          onContextMenu={(e) => e.preventDefault()}
        >
          {children}
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}

export function MenuItem({
  icon,
  children,
  onSelect,
  danger,
  checked,
  disabled,
  trailing,
}: {
  icon?: React.ReactNode;
  children: React.ReactNode;
  onSelect: () => void;
  danger?: boolean;
  checked?: boolean;
  disabled?: boolean;
  trailing?: React.ReactNode;
}) {
  return (
    <button className={`menu-item ${danger ? "danger" : ""}`} onClick={onSelect} disabled={disabled}>
      {icon !== undefined && <span className="menu-icon">{icon}</span>}
      <span className="menu-label">{children}</span>
      {trailing && <span className="menu-hint">{trailing}</span>}
      {checked && <Check size={14} className="menu-check" />}
    </button>
  );
}

export const MenuSeparator = () => <div className="menu-sep" />;
export const MenuLabel = ({ children }: { children: React.ReactNode }) => <div className="menu-section">{children}</div>;

// ---- small controls --------------------------------------------------------------------

export function Toggle({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button role="switch" aria-checked={on} aria-label={label} className={`toggle ${on ? "on" : ""}`} onClick={() => onChange(!on)}>
      <motion.span className="toggle-knob" layout transition={spring} />
    </button>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  id,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  id: string;
}) {
  return (
    <div className="segmented">
      {options.map((o) => (
        <button key={o.value} className={o.value === value ? "on" : ""} onClick={() => onChange(o.value)}>
          {o.value === value && <motion.span layoutId={`seg-${id}`} className="segmented-pill" transition={spring} />}
          <span>{o.label}</span>
        </button>
      ))}
    </div>
  );
}

/** A designed empty state: an icon in a soft well, a title, one sentence, the next action. */
export function Empty({ icon, title, children, action }: { icon: React.ReactNode; title: string; children?: React.ReactNode; action?: React.ReactNode }) {
  return (
    <div className="empty">
      <div className="empty-icon">{icon}</div>
      <h3>{title}</h3>
      {children && <p>{children}</p>}
      {action}
    </div>
  );
}

export function Stepper({
  value,
  min,
  max,
  step = 1,
  onChange,
  format,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
  format?: (v: number) => string;
}) {
  return (
    <div className="stepper">
      <IconButton size="small" label="Less" disabled={value <= min} onClick={() => onChange(Math.max(min, value - step))}>
        <Minus size={14} />
      </IconButton>
      <span className="stepper-value">{format ? format(value) : value}</span>
      <IconButton size="small" label="More" disabled={value >= max} onClick={() => onChange(Math.min(max, value + step))}>
        <Plus size={14} />
      </IconButton>
    </div>
  );
}

/** A drawn dropdown: a button that opens a menu of options. */
export function Select<T extends string>({
  value,
  options,
  onChange,
  width = 220,
  placeholder = "Choose…",
}: {
  value: T | null;
  options: { value: T; label: string; hint?: string }[];
  onChange: (v: T) => void;
  width?: number;
  placeholder?: string;
}) {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const current = options.find((o) => o.value === value);
  return (
    <>
      <button
        ref={btn}
        className={`select ${anchor ? "open" : ""}`}
        style={{ maxWidth: width }}
        onClick={() => setAnchor(anchor ? null : anchorFrom(btn.current!, { alignRight: true }))}
      >
        <span className={`select-label ${current ? "" : "faint"}`}>{current?.label ?? placeholder}</span>
        <ChevronDown size={14} className="select-caret" />
      </button>
      <Popover anchor={anchor} onClose={() => setAnchor(null)} width={Math.max(width, 200)} trigger={btn}>
        <div className="menu select-menu">
          {options.map((o) => (
            <MenuItem
              key={o.value}
              checked={o.value === value}
              trailing={o.hint}
              onSelect={() => {
                setAnchor(null);
                onChange(o.value);
              }}
            >
              {o.label}
            </MenuItem>
          ))}
        </div>
      </Popover>
    </>
  );
}

/** A textarea that grows with its text. */
export function AutoText({
  value,
  onChange,
  placeholder,
  minRows = 3,
  maxHeight = 360,
  className,
  autoFocus,
  onKeyDown,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  minRows?: number;
  maxHeight?: number;
  className?: string;
  autoFocus?: boolean;
  onKeyDown?: (e: React.KeyboardEvent<HTMLTextAreaElement>) => void;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight + 2, maxHeight)}px`;
  }, [value, maxHeight]);
  // Focus with the caret at the end (the autoFocus attribute puts it at the start).
  useLayoutEffect(() => {
    const el = ref.current;
    if (!autoFocus || !el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
    el.scrollTop = el.scrollHeight;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <textarea
      ref={ref}
      className={`auto-text ${className ?? ""}`}
      rows={minRows}
      style={{ minHeight: `calc(${minRows} * 1.5em + 20px)` }}
      value={value}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={onKeyDown}
    />
  );
}

/** A status dot: online, working, off, or trouble. */
export function Dot({ state }: { state: "ok" | "busy" | "off" | "error" }) {
  return <span className={`dot dot-${state}`} />;
}

// ---- toasts --------------------------------------------------------------------------------

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);
  return (
    <div className="toasts">
      <AnimatePresence>
        {toasts.map((t) => (
          <motion.div
            key={t.id}
            layout
            className="toast"
            initial={{ opacity: 0, y: 16, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.98, transition: { duration: 0.14 } }}
            transition={{ type: "spring", stiffness: 500, damping: 36 }}
          >
            <span>{t.text}</span>
            {t.action && (
              <button
                onClick={() => {
                  t.action!.run();
                  dismiss(t.id);
                }}
              >
                {t.action.label}
              </button>
            )}
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}

// ---- modal sheet ---------------------------------------------------------------------------

/** A centered sheet over a scrim, rising like a card. Escape and the scrim close it. */
export function Modal({
  open,
  onClose,
  children,
  width = 460,
  className,
}: {
  open: boolean;
  onClose: () => void;
  children: React.ReactNode;
  width?: number;
  className?: string;
}) {
  useEscape(open, onClose);
  return createPortal(
    <AnimatePresence>
      {open && (
        <div className="modal-layer">
          <motion.div
            className="modal-scrim"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0, transition: { duration: 0.16 } }}
            onPointerDown={onClose}
          />
          <motion.div
            className={`modal ${className ?? ""}`}
            style={{ width }}
            initial={{ opacity: 0, y: 18, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 10, scale: 0.985, transition: { duration: 0.14 } }}
            transition={{ type: "spring", stiffness: 420, damping: 36 }}
            onContextMenu={(e) => {
              if (!(e.target as HTMLElement).closest("input, textarea")) e.preventDefault();
            }}
          >
            {children}
          </motion.div>
        </div>
      )}
    </AnimatePresence>,
    document.body,
  );
}

/** Copies text, with a fallback for when the clipboard API isn't allowed. */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}
