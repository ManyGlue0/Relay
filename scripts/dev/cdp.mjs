// Tiny Chrome DevTools Protocol driver for the Relay webview.
// Usage: node cdp.mjs <cmd> [args...]
//   eval "<js>"            evaluate (awaits promises), print JSON result
//   shot <file.png>        screenshot of the page
//   click <x> <y>          mouse click at CSS px
//   move <x> <y>           move the mouse to CSS px (hover)
//   type "<text>"          insert text at focus
//   key <Key> [mods]       key press, e.g. key Enter, key n ctrl
import { writeFileSync } from "node:fs";

const PORT = 9333;
const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
const page = targets.find((t) => t.type === "page" && (process.env.TARGET ? t.url.includes(process.env.TARGET) : /localhost:1436|tauri.localhost/.test(t.url)));
if (!page) throw new Error("no page target");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let id = 0;
const pending = new Map();
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
  }
});
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const i = ++id;
    pending.set(i, (m) => (m.error ? reject(new Error(JSON.stringify(m.error))) : resolve(m.result)));
    ws.send(JSON.stringify({ id: i, method, params }));
  });

const [cmd, ...args] = process.argv.slice(2);
const modBits = (mods = "") => (mods.includes("alt") ? 1 : 0) | (mods.includes("ctrl") ? 2 : 0) | (mods.includes("shift") ? 8 : 0);

if (cmd === "eval") {
  const r = await send("Runtime.evaluate", { expression: args[0], awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) console.log("EXCEPTION", JSON.stringify(r.exceptionDetails.exception?.description ?? r.exceptionDetails));
  else console.log(JSON.stringify(r.result.value, null, 1));
} else if (cmd === "shot") {
  const r = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(args[0], Buffer.from(r.data, "base64"));
  console.log("saved", args[0]);
} else if (cmd === "click") {
  const [x, y] = args.map(Number);
  for (const type of ["mousePressed", "mouseReleased"])
    await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 });
  console.log("clicked");
} else if (cmd === "move") {
  const [x, y] = args.map(Number);
  await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
  console.log("moved");
} else if (cmd === "type") {
  await send("Input.insertText", { text: args[0] });
  console.log("typed");
} else if (cmd === "key") {
  const key = args[0];
  const modifiers = modBits(args[1]);
  const named = { Enter: 13, Escape: 27, Backspace: 8, Tab: 9, ArrowDown: 40, ArrowUp: 38, " ": 32 };
  const code = named[key] ?? key.toUpperCase().charCodeAt(0);
  const base = { key, windowsVirtualKeyCode: code, modifiers };
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...base, text: undefined });
  if (key === "Enter") await send("Input.dispatchKeyEvent", { type: "char", ...base, text: "\r" });
  await send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  console.log("key", key);
}
ws.close();
process.exit(0);
