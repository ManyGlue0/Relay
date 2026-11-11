// pnpm release:build: the installer for a GitHub release, signed for the
// in-app updater, plus the latest.json the updater reads. docs/RELEASING.md
// has the whole story.
//
// Needs Relay's private key, made with `pnpm tauri signer generate`, and its
// password: RELAY_SIGNING_KEY (a path or the key itself; defaults to
// %USERPROFILE%\.tauri\relay.key) and RELAY_SIGNING_KEY_PASSWORD, from this
// shell or from your Windows user environment variables. Tauri's generic
// TAURI_SIGNING_PRIVATE_KEY(_PASSWORD) work as a fallback.
//
// The first time, it also copies the public key (relay.key.pub) into
// src-tauri/tauri.conf.json, so every build after it can check updates.
//
// Writes target/release/bundle/nsis/latest.json; attach it to the release
// next to the installer. Optional: `-- --notes notes.md` puts a file's text in
// latest.json as the update's notes.
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = "ManyGlue0/Relay";
const confPath = join(root, "src-tauri", "tauri.conf.json");

/** A variable from this shell, else from the user's Windows environment. */
function userEnv(name) {
  if (process.env[name]) return process.env[name];
  try {
    const out = execFileSync("reg", ["query", "HKCU\\Environment", "/v", name], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    // "    NAME    REG_SZ    value": keep the value exactly, spaces and all.
    const m = out.match(new RegExp(`^\\s+${name}\\s+(REG_SZ|REG_EXPAND_SZ)    (.*?)\\r?$`, "m"));
    if (!m) return undefined;
    return m[1] === "REG_EXPAND_SZ" ? m[2].replace(/%([^%]+)%/g, (_, v) => process.env[v] ?? "") : m[2];
  } catch {
    return undefined;
  }
}

const key = userEnv("RELAY_SIGNING_KEY") ?? userEnv("TAURI_SIGNING_PRIVATE_KEY") ?? join(homedir(), ".tauri", "relay.key");
const password = userEnv("RELAY_SIGNING_KEY_PASSWORD") ?? userEnv("TAURI_SIGNING_PRIVATE_KEY_PASSWORD");
const keyIsFile = !key.includes("untrusted comment");
if (keyIsFile && !existsSync(key)) {
  console.error(`No private key at ${key}. Make one with: pnpm tauri signer generate -w "${key}"`);
  process.exit(1);
}
if (password === undefined) {
  console.error("Set RELAY_SIGNING_KEY_PASSWORD (in this shell or as a Windows user variable).");
  process.exit(1);
}

// The public key goes into every installer: the first time, take it from the key pair.
const conf = JSON.parse(readFileSync(confPath, "utf8"));
if (!conf.plugins?.updater?.pubkey) {
  const pubPath = `${key}.pub`;
  if (!keyIsFile || !existsSync(pubPath)) {
    console.error(`src-tauri/tauri.conf.json has no updater public key, and there's no ${pubPath} to take it from.`);
    process.exit(1);
  }
  const pubkey = readFileSync(pubPath, "utf8").trim();
  const text = readFileSync(confPath, "utf8").replace(/"pubkey":\s*""/, `"pubkey": "${pubkey}"`);
  writeFileSync(confPath, text);
  console.log("Added your public key to src-tauri/tauri.conf.json. Commit that change with this release.\n");
}

const build = spawnSync("pnpm", ["tauri", "build", "--config", "src-tauri/tauri.release.conf.json"], {
  cwd: root,
  stdio: "inherit",
  shell: true,
  env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY: key, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password },
});
if (build.status !== 0) process.exit(build.status ?? 1);

const { version } = JSON.parse(readFileSync(confPath, "utf8"));
const dir = join(root, "target", "release", "bundle", "nsis");
const installer = `Relay_${version}_x64-setup.exe`;
const sig = join(dir, `${installer}.sig`);
if (!existsSync(sig)) {
  console.error(`The build didn't sign the installer (${sig} is missing).`);
  process.exit(1);
}
const notesAt = process.argv.indexOf("--notes");
const latest = {
  version,
  notes: notesAt > 0 ? readFileSync(process.argv[notesAt + 1], "utf8").trim() : "",
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature: readFileSync(sig, "utf8").trim(),
      url: `https://github.com/${REPO}/releases/download/v${version}/${installer}`,
    },
  },
};
writeFileSync(join(dir, "latest.json"), JSON.stringify(latest, null, 2) + "\n");
console.log(`\nSigned ${installer}\nWrote ${join(dir, "latest.json")}\nAttach both to the v${version} release.`);
