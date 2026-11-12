import { openUrl } from "@tauri-apps/plugin-opener";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import "katex/dist/katex.min.css";
import { common, createLowlight } from "lowlight";
import { Check, Copy } from "lucide-react";
import { memo, useMemo, useState } from "react";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { escapeCurrency } from "../lib/dollars";
import { copyText } from "./ui";

const lowlight = createLowlight(common);
const ALIASES: Record<string, string> = { sh: "bash", shell: "bash", zsh: "bash", ps1: "powershell", ps: "powershell", js: "javascript", ts: "typescript", py: "python", rs: "rust", yml: "yaml", md: "markdown", html: "xml", jsonc: "json", cs: "csharp", "c++": "cpp" };

export function highlight(code: string, lang: string) {
  const name = ALIASES[lang] ?? lang;
  try {
    const tree = name && lowlight.registered(name) ? lowlight.highlight(name, code) : lowlight.highlightAuto(code, { subset: AUTO });
    return toJsxRuntime(tree, { Fragment, jsx, jsxs });
  } catch {
    return code;
  }
}
const AUTO = ["bash", "python", "javascript", "typescript", "json", "rust", "cpp", "java", "go", "sql", "xml", "css", "yaml"];

export const CodeBlock = memo(function CodeBlock({ code, lang }: { code: string; lang: string }) {
  const [copied, setCopied] = useState(false);
  // Very long blocks highlight once they've stopped growing much; plain text meanwhile is fine.
  const body = useMemo(() => (code.length > 60_000 ? code : highlight(code, lang)), [code, lang]);
  return (
    <div className="code-block">
      <div className="code-head">
        <span className="code-lang">{lang || "code"}</span>
        <button
          className="code-copy"
          onClick={() => {
            void copyText(code).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1400);
            });
          }}
        >
          {copied ? <Check size={13} strokeWidth={2.4} /> : <Copy size={13} />}
          <span>{copied ? "Copied" : "Copy"}</span>
        </button>
      </div>
      <pre>
        <code className="hljs">{body}</code>
      </pre>
    </div>
  );
});

function textOf(node: unknown): string {
  if (!node || typeof node !== "object") return "";
  const n = node as { type?: string; value?: string; children?: unknown[] };
  if (n.type === "text") return n.value ?? "";
  return (n.children ?? []).map(textOf).join("");
}

const components: Components = {
  pre({ node }) {
    const code = node?.children?.[0] as { properties?: { className?: string[] } } | undefined;
    const cls = code?.properties?.className?.find((c) => String(c).startsWith("language-"));
    const lang = cls ? String(cls).slice(9).toLowerCase() : "";
    return <CodeBlock code={textOf(code).replace(/\n$/, "")} lang={lang} />;
  },
  a({ href, children }) {
    return (
      <a
        href={href}
        onClick={(e) => {
          e.preventDefault();
          if (href && /^(https?:|mailto:)/i.test(href)) void openUrl(href);
        }}
        title={href}
      >
        {children}
      </a>
    );
  },
  table({ children }) {
    return (
      <div className="table-wrap">
        <table>{children}</table>
      </div>
    );
  },
  img({ src, alt }) {
    // Remote images are left as links: a chat shouldn't fetch from the web on its own.
    return (
      <a
        href={String(src ?? "")}
        onClick={(e) => {
          e.preventDefault();
          if (src && /^https?:/i.test(String(src))) void openUrl(String(src));
        }}
      >
        {alt || String(src ?? "image")}
      </a>
    );
  },
};

/** Models often write LaTeX as \( … \) and \[ … \]; remark-math wants dollars.
 *  Dollars that are money ("$5 and $10") are escaped so they stay text.
 *  Code (fenced and inline) is left alone. */
export function normalizeMath(md: string): string {
  if (!md.includes("\\(") && !md.includes("\\[") && !md.includes("$")) return md;
  return md
    .split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g)
    .map((part, i) => {
      if (i % 2 === 1) return part;
      return escapeCurrency(
        part.replace(/\\\[([\s\S]+?)\\\]/g, (_, m: string) => `$$${m}$$`).replace(/\\\(([\s\S]+?)\\\)/g, (_, m: string) => `$${m.trim()}$`),
      );
    })
    .join("");
}

const remarkPlugins = [remarkGfm, [remarkMath, { singleDollarTextMath: true }]] as never;
const rehypePlugins = [[rehypeKatex, { throwOnError: false, strict: "ignore" }]] as never;

const Block = memo(function Block({ text }: { text: string }) {
  return (
    <ReactMarkdown remarkPlugins={remarkPlugins} rehypePlugins={rehypePlugins} components={components}>
      {text}
    </ReactMarkdown>
  );
});

/** Splits markdown at blank lines outside code fences, so while a reply
 *  streams only its last block re-renders. */
export function splitBlocks(md: string): string[] {
  const lines = md.split("\n");
  const blocks: string[] = [];
  let cur: string[] = [];
  let fence: string | null = null;
  let math = false;
  for (const line of lines) {
    const t = line.trim();
    const f = /^(`{3,}|~{3,})/.exec(t);
    if (f) {
      if (!fence) fence = f[1][0].repeat(f[1].length);
      else if (t.startsWith(fence) && t.replace(/[`~]/g, "") === "") fence = null;
    } else if (!fence && t === "$$") {
      math = !math;
    }
    if (!fence && !math && t === "" && cur.length) {
      blocks.push(cur.join("\n"));
      cur = [];
      continue;
    }
    cur.push(line);
  }
  if (cur.length) blocks.push(cur.join("\n"));
  return blocks;
}

export const Markdown = memo(function Markdown({ text, streaming }: { text: string; streaming?: boolean }) {
  const md = normalizeMath(text);
  if (!streaming) {
    return (
      <div className="md">
        <Block text={md} />
      </div>
    );
  }
  const blocks = splitBlocks(md);
  return (
    <div className="md">
      {blocks.map((b, i) => (
        <Block key={i} text={b} />
      ))}
    </div>
  );
});
