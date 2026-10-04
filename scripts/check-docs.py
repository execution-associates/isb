#!/usr/bin/env python3
"""Check the documentation: front matter, relative links and anchors.

Every page under docs/ (docs/design/ excepted: it is never published) needs
front matter with a `title` and a `description`, and `order` must be a
number when given. Every folder needs an index.md. Every relative link in
those pages, README.md and SKILL.md must name a file that exists and, for a
Markdown target, an anchor that one of its headings produces (GitHub's
slugs). Published pages may not link into docs/design/, nor relatively to
anything outside docs/ (the site publishes docs/ alone).

Standard library only. Exits 1 and lists every problem when there are any.
"""

import re
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
DESIGN = DOCS / "design"
EXTRA = [ROOT / "README.md", ROOT / "SKILL.md"]

LINK = re.compile(r"(?<!\!)\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
IMAGE = re.compile(r"!\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
FENCE = re.compile(r"^\s*(```|~~~)")


def published(path: Path) -> bool:
    return path.suffix == ".md" and DESIGN not in path.parents


def strip_code(lines):
    """Lines with fenced blocks blanked and inline code removed."""
    out, fence = [], None
    for line in lines:
        m = FENCE.match(line)
        if fence:
            if m and m.group(1) == fence:
                fence = None
            out.append("")
            continue
        if m:
            fence = m.group(1)
            out.append("")
            continue
        out.append(re.sub(r"`+[^`]*`+", "", line))
    return out


def slug(text: str) -> str:
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)  # links keep their text
    text = text.replace("`", "").replace("*", "")
    text = unicodedata.normalize("NFC", text).strip().lower()
    text = "".join(c for c in text if c.isalnum() or c in " -_")
    return text.replace(" ", "-")


def anchors(path: Path, cache={}):
    if path not in cache:
        seen, found, fence = {}, set(), None
        for line in path.read_text(encoding="utf-8").splitlines():
            m = FENCE.match(line)
            if fence:
                fence = None if m and m.group(1) == fence else fence
                continue
            if m:
                fence = m.group(1)
                continue
            h = HEADING.match(line)
            if not h:
                continue
            base = slug(h.group(2))
            n = seen.get(base, 0)
            seen[base] = n + 1
            found.add(base if n == 0 else f"{base}-{n}")
        cache[path] = found
    return cache[path]


def front_matter(path: Path, text: str, problems):
    if not text.startswith("---\n"):
        problems.append(f"{rel(path)}: no front matter")
        return
    end = text.find("\n---\n", 4)
    if end < 0:
        problems.append(f"{rel(path)}: front matter is not closed")
        return
    fields = {}
    for line in text[4:end].splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        key, sep, value = line.partition(":")
        if not sep:
            problems.append(f"{rel(path)}: front matter line without a key: {line!r}")
            continue
        fields[key.strip()] = value.strip().strip("\"'")
    for key in ("title", "description"):
        if not fields.get(key):
            problems.append(f"{rel(path)}: front matter has no {key}")
    order = fields.get("order")
    if order is not None and not re.fullmatch(r"-?\d+(\.\d+)?", order):
        problems.append(f"{rel(path)}: order is not a number: {order!r}")
    h1 = [l for l in strip_code(text[end + 5:].splitlines()) if l.startswith("# ")]
    if h1:
        problems.append(f"{rel(path)}: has an H1 ({h1[0]!r}); the title comes from front matter")


def rel(path: Path) -> str:
    return str(path.relative_to(ROOT))


def check_links(path: Path, problems):
    lines = strip_code(path.read_text(encoding="utf-8").splitlines())
    for no, line in enumerate(lines, 1):
        for target in LINK.findall(line) + IMAGE.findall(line):
            if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.I) or target.startswith("//"):
                continue  # http:, https:, mailto:
            where = f"{rel(path)}:{no}"
            file_part, _, anchor = target.partition("#")
            dest = (path.parent / file_part).resolve() if file_part else path
            if not dest.exists():
                problems.append(f"{where}: {target}: no such file")
                continue
            if published(path) and DOCS in path.parents:
                if DESIGN in dest.parents:
                    problems.append(f"{where}: {target}: links into docs/design/, which is not published")
                elif DOCS not in dest.parents:
                    problems.append(
                        f"{where}: {target}: leaves docs/, which is all the site publishes; "
                        "link to https://github.com/execution-associates/isb/blob/main/... instead"
                    )
            if anchor and dest.suffix == ".md" and anchor not in anchors(dest):
                problems.append(f"{where}: {target}: no heading makes #{anchor}")


def main() -> int:
    problems = []
    pages = sorted(p for p in DOCS.rglob("*.md") if published(p))
    if not (DOCS / "index.md").exists():
        problems.append("docs/index.md is missing")
    for folder in sorted({p.parent for p in pages}):
        if not (folder / "index.md").exists():
            problems.append(f"{rel(folder)}/ has no index.md")
    for page in pages:
        front_matter(page, page.read_text(encoding="utf-8"), problems)
        check_links(page, problems)
    for extra in EXTRA:
        if extra.exists():
            check_links(extra, problems)
    for p in problems:
        print(p)
    print(f"check-docs: {len(pages)} pages, {len(problems)} problem(s)", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
