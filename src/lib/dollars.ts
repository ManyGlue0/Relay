/** Tells money from math. remark-math reads any `$…$` as TeX, so "$5 and $10"
 *  became a formula. This escapes the dollars that don't pair up the way TeX
 *  writers use them (the rule Pandoc uses, plus a stricter one for numbers):
 *
 *  - an opening `$` has a non-space right after it;
 *  - the closing `$` is the next `$`, has a non-space before it and no digit
 *    after it ("$5-$10" is two prices); an inline formula never holds a bare `$`;
 *  - a formula that starts with a digit stays on one line and looks like math
 *    (an operator or command, or no spaces at all: "$2x + 1$", "$100$");
 *  - `$$…$$` and escaped dollars are left alone.
 *
 *  Call it on prose only: code spans and fences are the caller's to skip. */
export function escapeCurrency(text: string): string {
  if (!text.includes("$")) return text;
  let out = "";
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === "\\") {
      out += text.slice(i, i + 2);
      i += 2;
      continue;
    }
    if (c !== "$") {
      out += c;
      i++;
      continue;
    }
    if (text[i + 1] === "$") {
      // Display math runs to its closing $$.
      const end = text.indexOf("$$", i + 2);
      const stop = end < 0 ? text.length : end + 2;
      out += text.slice(i, stop);
      i = stop;
      continue;
    }
    const close = closing(text, i);
    if (close < 0) {
      out += "\\$";
      i++;
    } else {
      out += text.slice(i, close + 1);
      i = close + 1;
    }
  }
  return out;
}

const SPACE = /\s/;
const DIGIT = /[0-9]/;
const MATHY = /[\\^_{}=+*/<>|]/;

/** Where the formula opened at `open` closes, or -1 if that `$` is just a dollar. */
function closing(t: string, open: number): number {
  const first = t[open + 1];
  if (!first || SPACE.test(first)) return -1;
  for (let j = open + 1; j < t.length; j++) {
    const c = t[j];
    if (c === "\\") {
      j++;
      continue;
    }
    if (c === "\n" && t[j + 1] === "\n") return -1; // the paragraph ended
    if (c !== "$") continue;
    if (t[j + 1] === "$" || SPACE.test(t[j - 1]) || DIGIT.test(t[j + 1] ?? "")) return -1;
    if (DIGIT.test(first)) {
      const body = t.slice(open + 1, j);
      if (body.includes("\n") || (SPACE.test(body) && !MATHY.test(body))) return -1;
    }
    return j;
  }
  return -1;
}
