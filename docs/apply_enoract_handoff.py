#!/usr/bin/env python3
"""Apply the esg raw-HTML handoff to enoract's crawler, by exact-anchor edits.

Idempotent: re-running is a no-op once applied. Run from an enoract worktree:

    python /home/enor/e-shop-graph/docs/apply_enoract_handoff.py \
        enoract/console/knowledge/sources/web/crawler.py

Design: the handoff is driven by the env var ESG_RAW_HTML_DIR, read inside
`_process` (the single funnel every crawl*/_run_loop path goes through). This
needs only TWO edits — an import and the persist block — and touches NO public
signature or call site, so there are no name-collision anchors across the
several crawl* functions. When the env var is set, each rendered page is written
to $ESG_RAW_HTML_DIR/<sha256(url)[:16]>.html; unset = exact original behavior.
See docs/enoract-integration.md.
"""
import sys

EDITS = [
    (
        "import asyncio\nimport os\nimport re\nimport sys\n",
        "import asyncio\nimport os\nimport re\nimport sys\nimport hashlib\nfrom pathlib import Path\n",
        "imports",
    ),
    (
        "            html = await _rescue_if_spa(url, html)\n",
        "            html = await _rescue_if_spa(url, html)\n"
        "            # esg handoff: when ESG_RAW_HTML_DIR is set, persist rendered\n"
        "            # HTML (one file per page) for the product-graph engine. Unset\n"
        "            # = no-op. See docs/enoract-integration.md.\n"
        "            _esg_dir = os.environ.get(\"ESG_RAW_HTML_DIR\")\n"
        "            if _esg_dir and html:\n"
        "                _d = Path(_esg_dir)\n"
        "                _d.mkdir(parents=True, exist_ok=True)\n"
        "                _fname = hashlib.sha256(url.encode()).hexdigest()[:16] + \".html\"\n"
        "                (_d / _fname).write_text(html, encoding=\"utf-8\")\n",
        "_process persist",
    ),
]


def main(path: str) -> int:
    src = open(path, encoding="utf-8").read()
    applied, skipped = [], []
    for anchor, repl, desc in EDITS:
        if repl in src:
            skipped.append(desc)
            continue
        count = src.count(anchor)
        if count != 1:
            print(f"ABORT: anchor for {desc!r} matched {count} times (need 1)")
            return 1
        src = src.replace(anchor, repl, 1)
        applied.append(desc)
    open(path, "w", encoding="utf-8").write(src)
    print(f"applied: {applied or '(none)'}")
    print(f"already-present: {skipped or '(none)'}")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
