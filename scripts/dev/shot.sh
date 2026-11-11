#!/usr/bin/env bash
# In-page screenshot with a stand-in for the acrylic (CDP can't capture it).
# Usage: scripts/dev/shot.sh <out.png>
cd "$(dirname "$0")/../.."
node scripts/dev/cdp.mjs eval "(() => { let s = document.getElementById('shot-bg'); if (!s) { s = document.createElement('style'); s.id = 'shot-bg'; document.head.appendChild(s); } s.textContent = ':root[data-theme=dark] body{background:#1f1d1e} :root:not([data-theme=dark]) body{background:#f1eff0}'; return 1 })()" >/dev/null
node scripts/dev/cdp.mjs shot "$1"
