// A tiny MCP server over stdio, for testing Relay's tools without installing anything.
// In Relay: Settings, Tools, Add server, command: node <path to this file>
// Tools: add (two numbers), now (the date and time), shout (text in capitals), fail (always errors).
import { createInterface } from "node:readline";

const tools = [
  {
    name: "add",
    description: "Adds two numbers and returns the sum.",
    inputSchema: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
  },
  { name: "now", description: "Returns the current date and time on this computer.", inputSchema: { type: "object", properties: {} } },
  {
    name: "shout",
    description: "Returns the text in capital letters.",
    inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
  },
  { name: "fail", description: "Always fails (for testing errors).", inputSchema: { type: "object", properties: {} } },
];

const send = (msg) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...msg }) + "\n");
const text = (t, isError = false) => ({ content: [{ type: "text", text: t }], isError });

createInterface({ input: process.stdin }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = msg;
  if (id === undefined) return; // notifications
  switch (method) {
    case "initialize":
      return send({ id, result: { protocolVersion: params?.protocolVersion ?? "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "relay-test", version: "1.0.0" } } });
    case "ping":
      return send({ id, result: {} });
    case "tools/list":
      return send({ id, result: { tools } });
    case "tools/call": {
      const a = params?.arguments ?? {};
      switch (params?.name) {
        case "add":
          return send({ id, result: text(String(Number(a.a) + Number(a.b))) });
        case "now":
          return send({ id, result: text(new Date().toString()) });
        case "shout":
          return send({ id, result: text(String(a.text ?? "").toUpperCase()) });
        case "fail":
          return send({ id, result: text("This tool always fails.", true) });
        default:
          return send({ id, error: { code: -32602, message: `Unknown tool ${params?.name}` } });
      }
    }
    default:
      return send({ id, error: { code: -32601, message: `Method not found: ${method}` } });
  }
});
process.stderr.write("relay-test MCP server ready\n");
