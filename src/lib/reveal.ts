/** How a reply's text appears while it streams (Settings → General). The
 *  server always streams; this only changes what's on screen. */
export type Reveal = "token" | "sentence" | "paragraph" | "whole";

/** A run of these ends a sentence: "Done.", "Hello!!!", "Really?!", "No thanks...". */
const END = /[.!?…。！？]/;
/** What may follow the run before the space: closing quotes, brackets, emphasis. */
const CLOSE = /[)\]"'”’»*_`]/;

/** The part of a streaming text that's ready to show: up to the last whole
 *  sentence or paragraph. Code blocks show a line at a time either way. */
export function revealed(text: string, mode: Reveal): string {
  if (mode === "token") return text;
  if (mode === "whole") return "";
  let cut = 0;
  let fenced = false;
  let lineStart = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\n") {
      const line = text.slice(lineStart, i);
      const fence = /^\s{0,3}(```|~~~)/.test(line);
      if (fence) fenced = !fenced;
      // In code (and on its fence lines), every line; in prose, every line
      // for sentences (headings, list items), blank lines for paragraphs.
      if (fenced || fence || mode === "sentence" || line.trim() === "") cut = i + 1;
      lineStart = i + 1;
      continue;
    }
    if (mode === "sentence" && !fenced && END.test(ch)) {
      // The whole run ("!!!", "...", "?!") and what closes it, then a space:
      // only then is the sentence known to be over.
      let j = i + 1;
      while (j < text.length && END.test(text[j])) j++;
      while (j < text.length && CLOSE.test(text[j])) j++;
      if (j < text.length && (text[j] === " " || text[j] === "\t" || text[j] === "\n")) cut = j;
      i = j - 1;
    }
  }
  return text.slice(0, cut);
}
