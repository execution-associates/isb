# /// script
# requires-python = ">=3.11"
# ///
"""Turn a `tmux capture-pane -e` dump into a styled HTML page: a terminal
window on the web UI's background, sized to the same 1280x800 viewport as
the browser screenshots, so terminal and web cells match in the README grid.

usage: ansi2html.py IN.ansi OUT.html TITLE
"""
import html
import re
import sys

# A palette close to the web UI's (dark plum background, pink accent).
PALETTE = [
    "#2a2133", "#f0768b", "#7fd8a4", "#f2c879", "#8aa8ff", "#e08ce8", "#7fd4e0", "#d9cfe3",
    "#5a4d66", "#ff8fa3", "#9bf0bd", "#ffd994", "#a8bfff", "#f0a8f5", "#9be8f2", "#f7f2fb",
]
FG, BG = "#e6dcef", "#17101e"


def xterm256(n: int) -> str:
    if n < 16:
        return PALETTE[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[(n // 6) % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def convert(text: str) -> str:
    out, st = [], {}
    pos = 0
    for m in re.finditer(r"\x1b\[([0-9;:]*)m", text):
        out.append(span(text[pos:m.start()], st))
        pos = m.end()
        codes = [int(c) if c else 0 for c in re.split(r"[;:]", m.group(1))] or [0]
        i = 0
        while i < len(codes):
            c = codes[i]
            if c == 0:
                st = {}
            elif c == 1:
                st["b"] = True
            elif c == 2:
                st["dim"] = True
            elif c == 22:
                st.pop("b", None); st.pop("dim", None)
            elif c == 3:
                st["i"] = True
            elif c == 23:
                st.pop("i", None)
            elif c == 4:
                st["u"] = True
            elif c == 24:
                st.pop("u", None)
            elif c == 7:
                st["rev"] = True
            elif c == 27:
                st.pop("rev", None)
            elif 30 <= c <= 37:
                st["fg"] = PALETTE[c - 30]
            elif 90 <= c <= 97:
                st["fg"] = PALETTE[c - 90 + 8]
            elif c == 39:
                st.pop("fg", None)
            elif 40 <= c <= 47:
                st["bg"] = PALETTE[c - 40]
            elif 100 <= c <= 107:
                st["bg"] = PALETTE[c - 100 + 8]
            elif c == 49:
                st.pop("bg", None)
            elif c in (38, 48) and i + 1 < len(codes):
                key = "fg" if c == 38 else "bg"
                if codes[i + 1] == 5 and i + 2 < len(codes):
                    st[key] = xterm256(codes[i + 2]); i += 2
                elif codes[i + 1] == 2 and i + 4 < len(codes):
                    st[key] = "#%02x%02x%02x" % tuple(codes[i + 2:i + 5]); i += 4
            i += 1
    out.append(span(text[pos:], st))
    return "".join(out)


def span(s: str, st: dict) -> str:
    if not s:
        return ""
    s = html.escape(s)
    if not st:
        return s
    fg, bg = st.get("fg"), st.get("bg")
    if st.get("rev"):
        fg, bg = bg or BG, fg or FG
    css = []
    if fg: css.append(f"color:{fg}")
    if bg: css.append(f"background:{bg}")
    if st.get("b"): css.append("font-weight:700")
    if st.get("dim"): css.append("opacity:.6")
    if st.get("i"): css.append("font-style:italic")
    if st.get("u"): css.append("text-decoration:underline")
    return f'<span style="{";".join(css)}">{s}</span>'


def main() -> None:
    src, dst, title = sys.argv[1], sys.argv[2], sys.argv[3]
    text = open(src, encoding="utf-8").read().rstrip("\n ")
    lines = text.split("\n")
    cols = max(len(re.sub(r"\x1b\[[0-9;:]*m", "", l)) for l in lines)
    body = convert(text)
    page = f"""<!doctype html><html><head><meta charset="utf-8"><title>{html.escape(title)}</title>
<style>
html,body{{margin:0;height:100%}}
body{{background:radial-gradient(120% 90% at 20% 0%,#3a2347 0%,#1d1226 55%,#140c1a 100%);
  display:flex;align-items:center;justify-content:center;font-family:Menlo,"DejaVu Sans Mono",monospace}}
.win{{background:{BG};border:1px solid #4a3858;border-radius:12px;box-shadow:0 24px 60px rgba(0,0,0,.55);overflow:hidden}}
.bar{{height:34px;display:flex;align-items:center;gap:8px;padding:0 14px;background:#221829;border-bottom:1px solid #33263d;position:relative}}
.dot{{width:12px;height:12px;border-radius:50%}}
.t{{position:absolute;left:0;right:0;text-align:center;color:#a596b3;font:500 13px/34px -apple-system,system-ui,sans-serif;pointer-events:none}}
pre{{margin:0;padding:18px 22px 22px;color:{FG};font-size:var(--fs);line-height:1.22;white-space:pre}}
</style></head><body>
<div class="win"><div class="bar"><span class="dot" style="background:#ff5f57"></span><span class="dot" style="background:#febc2e"></span><span class="dot" style="background:#28c840"></span><span class="t">{html.escape(title)}</span></div>
<pre id="p">{body}</pre></div>
<script>
// Fit the terminal into the 1280x800 viewport with a margin: the widest
// line sets the font size, capped so short captures don't balloon.
const cols={cols}, rows={len(lines)};
const fs=Math.min(21,(1280-150)/(cols*0.602),(800-150)/(rows*1.22));
document.documentElement.style.setProperty('--fs',fs.toFixed(2)+'px');
</script></body></html>"""
    open(dst, "w", encoding="utf-8").write(page)


if __name__ == "__main__":
    main()
