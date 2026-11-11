#!/usr/bin/env bash
# The README screenshots in docs/images, from a running Relay Dev
# (`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9333" pnpm dev:app`).
#
# In-page captures: the webview draws the page itself, so the image is exactly
# the window (no invisible resize border) and nothing else on screen can end up
# in it. A flat neutral color stands in for the acrylic; finish-shot.py then
# rounds the corners and adds the window's hairline. Needs Pillow.
#
# Set it up first: a provider or two (one unreachable, for the Offline section),
# a few real chats, and the chat to show.
#   Usage: scripts/dev/readme-shots.sh <chat id>
set -e
cd "$(dirname "$0")/../.."
CHAT="$1"
[ -n "$CHAT" ] || { echo "usage: $0 <chat id>"; exit 1; }
IMG=docs/images
TMP="$(mktemp -d)"
cdp() { node scripts/dev/cdp.mjs "$@"; }
finish() { python scripts/dev/finish-shot.py "$TMP/$1.png" "$IMG/$1.png" "$2"; }

# A neutral color where the acrylic would be; the scroll-to-latest arrow hidden.
cdp eval "(() => { let s = document.getElementById('shot-bg'); if (!s) { s = document.createElement('style'); s.id = 'shot-bg'; document.head.appendChild(s); } s.textContent = ':root[data-theme=dark] body{background:#1e1e20!important} :root:not([data-theme=dark]) body{background:#f0f0f2!important} .to-bottom{visibility:hidden!important}'; return 1 })()" >/dev/null

chat() {
  cdp eval "(async()=>{const s=window.relay.getState(); await s.updateSettings({theme:'$1'}); s.closeSettings(); await s.openChat('$CHAT'); await new Promise(r=>setTimeout(r,900)); document.querySelector('.messages').scrollTop=0; await new Promise(r=>setTimeout(r,300)); return 1})()" >/dev/null
}

chat dark
cdp shot "$TMP/chat-dark.png" >/dev/null
finish chat-dark dark

chat light
cdp shot "$TMP/chat-light.png" >/dev/null
finish chat-light light

# The model picker, with the Offline section open.
chat dark
cdp eval "(async()=>{await new Promise(r=>setTimeout(r,4200)); document.querySelector('.composer-bar .chip').click(); await new Promise(r=>setTimeout(r,1500)); const h=document.querySelector('.model-offline-head'); if (h && h.getAttribute('aria-expanded')!=='true') h.click(); await new Promise(r=>setTimeout(r,600)); return 1})()" >/dev/null
cdp shot "$TMP/picker-dark.png" >/dev/null
finish picker-dark dark

# Settings → Models.
cdp eval "(async()=>{const c=document.querySelector('.composer-bar .chip'); if (document.querySelector('.model-list')) c.click(); await new Promise(r=>setTimeout(r,400)); window.relay.getState().openSettings('models'); await new Promise(r=>setTimeout(r,900)); return 1})()" >/dev/null
cdp shot "$TMP/models-dark.png" >/dev/null
finish models-dark dark

# Back to how it was.
cdp eval "(async()=>{document.getElementById('shot-bg')?.remove(); window.relay.getState().closeSettings(); await window.relay.getState().updateSettings({theme:'system'}); return 1})()" >/dev/null
rm -rf "$TMP"
