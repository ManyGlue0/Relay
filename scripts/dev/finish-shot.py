# For README screenshots (scripts/dev/readme-shots.sh). Needs Pillow (pip install pillow).
# Rounds a window screenshot's corners (Windows 11: 8 px) and draws the 1 px
# hairline Windows puts around a window. Renders at 4x and scales down, so the
# corners are smooth.  Usage: finish-shot.py <in.png> <out.png> <dark|light>
import sys
from PIL import Image, ImageDraw

src, out, theme = sys.argv[1], sys.argv[2], sys.argv[3]
im = Image.open(src).convert("RGBA")
w, h = im.size
S, R = 4, 8
border = (47, 47, 47, 255) if theme == "dark" else (206, 206, 206, 255)

mask = Image.new("L", (w * S, h * S), 0)
ImageDraw.Draw(mask).rounded_rectangle((0, 0, w * S - 1, h * S - 1), radius=R * S, fill=255)
mask = mask.resize((w, h), Image.LANCZOS)

ring = Image.new("RGBA", (w * S, h * S), (0, 0, 0, 0))
ImageDraw.Draw(ring).rounded_rectangle((0, 0, w * S - 1, h * S - 1), radius=R * S, outline=border, width=S)
ring = ring.resize((w, h), Image.LANCZOS)

im.putalpha(mask)
im = Image.alpha_composite(im, ring)
im.save(out, optimize=True)
print(out, im.size)
