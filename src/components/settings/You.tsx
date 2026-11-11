import { motion } from "framer-motion";
import { ChevronRight } from "lucide-react";
import { useEffect, useState } from "react";
import { api, type Profile } from "../../lib/api";
import { spring } from "../../lib/motion";
import { useStore } from "../../lib/store";
import { AutoText, Toggle } from "../ui";
import { Group, Page, Row, TextRow } from "./kit";

/** Who you are, for the model: sent at the start of every chat (if you like). */
export function You() {
  const s = useStore((st) => st.settings);
  const update = useStore((st) => st.updateSettings);
  if (!s) return null;
  const p = s.profile;
  const save = (patch: Partial<Profile>) => void update({ profile: { ...p, ...patch } });

  return (
    <Page title="You" intro="Tell models who they're talking to. Relay adds this to the start of every chat, so answers fit you.">
      <Group title="About you">
        <TextRow label="Name" value={p.name} placeholder="Your name" onSave={(name) => save({ name })} />
        <TextRow label="What should models call you?" hint="If it's not your name" value={p.nickname} placeholder="A nickname" onSave={(nickname) => save({ nickname })} />
        <TextRow label="Pronouns" value={p.pronouns} placeholder="e.g. she/her, they/them" onSave={(pronouns) => save({ pronouns })} />
        <TextRow label="Where you are" hint="For local answers: units, time zones, places" value={p.location} placeholder="City, country" onSave={(location) => save({ location })} />
        <TextRow label="What you do" value={p.occupation} placeholder="e.g. Software engineer" onSave={(occupation) => save({ occupation })} />
      </Group>

      <Group title="More about you" hint="Interests, what you're working on, what you know well">
        <LongText value={p.about} placeholder="I build desktop apps in Rust and React, I'm learning Japanese, and I like concise explanations with examples." onSave={(about) => save({ about })} />
      </Group>

      <Group title="How you like answers" hint="Tone, length, format">
        <LongText value={p.style} placeholder="Be direct. Skip the preamble. Use code blocks for code and tables to compare options." onSave={(style) => save({ style })} />
      </Group>

      <Group>
        <TextRow label="Answer in" hint="Leave it empty to answer in the language you write in" value={p.language} placeholder="e.g. Italian" onSave={(language) => save({ language })} />
      </Group>

      <Group title="Sharing">
        <Row label="Share this with models" hint="Off: models know nothing about you">
          <Toggle label="Share this with models" on={s.shareProfile} onChange={(shareProfile) => void update({ shareProfile })} />
        </Row>
        <Row label="Tell models the date and time" hint="Each message carries the time you sent it, so “today”, “this week” and deadlines make sense">
          <Toggle label="Tell models the date and time" on={s.shareDate} onChange={(shareDate) => void update({ shareDate })} />
        </Row>
      </Group>

      <Preview deps={[s.profile, s.shareProfile, s.shareDate]} />
    </Page>
  );
}

function LongText({ value, placeholder, onSave }: { value: string; placeholder: string; onSave: (v: string) => void }) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <div className="long-text" onBlur={() => draft !== value && onSave(draft)}>
      <AutoText value={draft} onChange={setDraft} placeholder={placeholder} minRows={3} />
    </div>
  );
}

/** Exactly what goes to the model, so nothing is a surprise. */
function Preview({ deps }: { deps: unknown[] }) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  useEffect(() => {
    if (open) void api.systemPromptPreview(null).then(setText);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, ...deps]);
  return (
    <section className="group">
      <button className="disclosure" onClick={() => setOpen(!open)}>
        <motion.span animate={{ rotate: open ? 90 : 0 }} transition={spring} className="disclosure-chev">
          <ChevronRight size={14} />
        </motion.span>
        What models see
      </button>
      {open && <pre className="prompt-preview">{text || "Nothing: models start every chat knowing nothing about you."}</pre>}
    </section>
  );
}
