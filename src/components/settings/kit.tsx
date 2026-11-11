import { useState } from "react";
import { Button } from "../ui";

/** A settings page: its title, an optional action, one line of context. */
export function Page({ title, action, intro, children }: { title: string; action?: React.ReactNode; intro?: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="set-page">
      <div className="set-head">
        <h2>{title}</h2>
        {action && <div className="set-head-action">{action}</div>}
      </div>
      {intro && <p className="set-intro">{intro}</p>}
      {children}
    </div>
  );
}

export function Group({ title, hint, action, children }: { title?: string; hint?: string; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="group">
      {(title || action) && (
        <div className="group-title">
          {title && <span>{title}</span>}
          {hint && <span className="group-hint">{hint}</span>}
          {action && <span className="group-action">{action}</span>}
        </div>
      )}
      <div className="group-card">{children}</div>
    </section>
  );
}

export function Row({
  label,
  hint,
  children,
  stack,
  onClick,
  className,
}: {
  label: React.ReactNode;
  hint?: React.ReactNode;
  children?: React.ReactNode;
  /** Control under the text (wide inputs). */
  stack?: boolean;
  onClick?: () => void;
  className?: string;
}) {
  return (
    <div className={`row ${stack ? "stack" : ""} ${onClick ? "clickable" : ""} ${className ?? ""}`} onClick={onClick}>
      <div className="row-text">
        <div className="row-label">{label}</div>
        {hint && <div className="row-hint">{hint}</div>}
      </div>
      {children !== undefined && <div className="row-control">{children}</div>}
    </div>
  );
}

/** A text input row that saves when you leave it (or press Enter). */
export function TextRow({
  label,
  hint,
  value,
  placeholder,
  onSave,
  width = 260,
}: {
  label: string;
  hint?: string;
  value: string;
  placeholder?: string;
  onSave: (v: string) => void;
  width?: number;
}) {
  const [draft, setDraft] = useState(value);
  const [focused, setFocused] = useState(false);
  const shown = focused ? draft : value;
  return (
    <Row label={label} hint={hint}>
      <input
        className="input"
        style={{ width }}
        value={shown}
        placeholder={placeholder}
        onFocus={() => {
          setDraft(value);
          setFocused(true);
        }}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => {
          setFocused(false);
          if (draft !== value) onSave(draft);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
      />
    </Row>
  );
}

/** "Delete" that asks in place: the button turns into the question. */
export function ConfirmButton({ label, question, confirm, onConfirm }: { label: React.ReactNode; question: string; confirm: string; onConfirm: () => void }) {
  const [asking, setAsking] = useState(false);
  if (!asking)
    return (
      <Button variant="default" className="danger-text" onClick={() => setAsking(true)}>
        {label}
      </Button>
    );
  return (
    <div className="inline-confirm">
      <span>{question}</span>
      <Button
        variant="danger"
        onClick={() => {
          setAsking(false);
          onConfirm();
        }}
      >
        {confirm}
      </Button>
      <Button variant="ghost" onClick={() => setAsking(false)}>
        Cancel
      </Button>
    </div>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: React.ReactNode; children: React.ReactNode }) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

/** A number box where empty means "the model's default". */
export function NumberInput({
  value,
  onChange,
  min,
  max,
  step,
  placeholder = "Default",
  width = 110,
}: {
  value: number | null;
  onChange: (v: number | null) => void;
  min?: number;
  max?: number;
  step?: number;
  placeholder?: string;
  width?: number;
}) {
  const [text, setText] = useState(value == null ? "" : String(value));
  return (
    <input
      className="input num"
      style={{ width }}
      inputMode="decimal"
      value={text}
      placeholder={placeholder}
      onChange={(e) => {
        setText(e.target.value);
        const t = e.target.value.trim();
        if (!t) return onChange(null);
        const n = Number(t);
        if (Number.isFinite(n)) onChange(Math.min(max ?? n, Math.max(min ?? n, n)));
      }}
      onBlur={() => setText(value == null ? "" : String(value))}
      step={step}
    />
  );
}
