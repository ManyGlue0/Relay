# Releasing Relay

Every release ships two files on GitHub: the installer and `latest.json`. The
installer is what people download by hand; `latest.json` is what the
**Settings → About → Updates** button reads. It works the same way as
Holster's updater.

## How the updater works

Relay uses [`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/)
(code in `src-tauri/src/update.rs`, the card in
`src/components/settings/About.tsx`).

1. **Check.** Relay downloads
   `https://github.com/ManyGlue0/Relay/releases/latest/download/latest.json`
   (the endpoint in `src-tauri/tauri.conf.json` → `plugins.updater`) and
   compares its `version` with its own.
2. **Verify.** If it's newer, Relay downloads the installer named in `url`
   and checks it against `signature`, using the **public key** built into
   every Relay (`plugins.updater.pubkey`). A file that doesn't match is
   refused.
3. **Install.** Relay saves its window, stops its tool servers, removes its
   tray icon and exits; the installer runs in passive mode (a progress bar, no
   questions) and starts the new Relay. Chats, settings and keys are kept.

The signature comes from a **private key** that never leaves the
maintainer's PC. Anyone can check a signature with the public key; only the
private key can make one, so not even someone who took over the GitHub
account could push an update Relay would install.

| | Where | Changes |
|---|---|---|
| Private key | `%USERPROFILE%\.tauri\relay.key`, password-protected, plus a backup | Never |
| Public key | `tauri.conf.json` → `plugins.updater.pubkey`, so inside every installer | Never |
| Signature | `latest.json` on each release (the build also writes a `.sig` file) | Every build |

The key pair is Relay's own (one per product, separate from Holster's). It
isn't Windows code signing: SmartScreen never sees it, and it costs nothing.

Builds without the public key (a plain `pnpm tauri build` before the first
release, or Relay Dev, whose `tauri.dev.json` clears it) don't update
themselves; the Updates card says so.

## One-time setup

1. **Make the key pair**, in your own terminal so the password stays yours:

   ```powershell
   cd path\to\Relay
   pnpm tauri signer generate -w "$env:USERPROFILE\.tauri\relay.key"
   ```

   It asks for a password and writes `relay.key` (private) and
   `relay.key.pub` (public) to `%USERPROFILE%\.tauri\`, outside the repo.

2. **Back up `relay.key` and its password** (a password manager, a backup
   drive). If either is lost, no installed Relay can be updated by the button
   again: everyone would have to install a new version, with a new public
   key, by hand.

3. **Save the password for release builds** as a Windows user variable. This
   prompt keeps it out of PowerShell's history file:

   ```powershell
   $p = Read-Host "Key password" -AsSecureString
   [Environment]::SetEnvironmentVariable("RELAY_SIGNING_KEY_PASSWORD", [Net.NetworkCredential]::new("", $p).Password, "User")
   Remove-Variable p
   ```

   `"User"` is the scope (your Windows account), not a name to replace.
   Check without showing it:
   `[bool][Environment]::GetEnvironmentVariable("RELAY_SIGNING_KEY_PASSWORD", "User")`
   → `True`. Open a new terminal afterwards so it sees the variable (the
   build script also reads it from the registry). Optional:
   `RELAY_SIGNING_KEY` if the key lives somewhere other than the default
   path.

4. **The public key** goes into `src-tauri/tauri.conf.json` by itself: the
   first `pnpm release:build` copies `relay.key.pub` there and says so.
   Commit that change.

`.gitignore` refuses `*.key` and `*.key.pub`, so a key copied into the repo
by mistake can't be committed.

## Each release

1. **Bump the version** in all three places: `package.json`,
   `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`. The updater only
   installs a higher version.

2. **Build, signed:**

   ```powershell
   pnpm release:build
   ```

   That's `scripts/release-build.mjs`: it reads the key and password, runs
   `pnpm tauri build` with `src-tauri/tauri.release.conf.json` (which turns on
   `createUpdaterArtifacts`), and writes, in `target\release\bundle\nsis\`:

   - `Relay_<version>_x64-setup.exe`, the installer
   - `Relay_<version>_x64-setup.exe.sig`, its signature
   - `latest.json`, with the version, the download URL and that signature

   `pnpm release:build -- --notes notes.md` also puts a file's text in
   `latest.json` as the update's notes.

3. **Commit and push** whatever changed (at least the version bump):

   ```powershell
   git add -A
   git commit -m "Relay <version>"
   git push
   ```

4. **Publish both files**, marked latest:

   ```powershell
   gh release create v<version> `
     target/release/bundle/nsis/Relay_<version>_x64-setup.exe `
     target/release/bundle/nsis/latest.json `
     --target main --title "Relay <version>" --notes-file notes.md --latest
   ```

   A release without `latest.json` makes the Updates card report an error,
   because `releases/latest/download/latest.json` would be missing.

5. **Check it from outside:**
   `https://github.com/ManyGlue0/Relay/releases/latest/download/latest.json`
   should show the new version.

## If something goes wrong

- **"Couldn't check for updates"**: the latest release is missing
  `latest.json`, or there's no network.
- **The release build stops at signing**: the password variable is missing
  or wrong. Set it again (step 3 of the setup).
- **The private key is lost or leaked**: make a new pair, put the new public
  key in `tauri.conf.json`, and release. Installed copies still trust only the
  old key, so tell people to install that release by hand once.
