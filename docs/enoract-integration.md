# enoract integration — raw HTML handoff

esg builds the product graph from **rendered** HTML. For Layer-3 stores (91app,
全聯, 7-11 —商品在 JS-fetched API), only enoract's crawler reaches the data,
because it runs chromium SPA fallback. The handoff is by **file system**: the
crawler writes one rendered HTML per page; esg streams them via
`build_from_files` (memory-bounded mmap, see `esg-extract/src/lib.rs`).

This avoids both OOM (a 10k-page crawl at 2MB/page would be ~20GB if held in
memory) and shipping GB of strings across a PyO3 boundary.

## enoract-side change (minimal, opt-in)

Target: `enoract/console/knowledge/sources/web/crawler.py`. Add an optional
`raw_html_dir: Path | None = None` parameter, threaded `crawl` → `_run_loop` →
`_process`. When set, `_process` writes the rendered HTML (the value live at
line ~504, AFTER `_rescue_if_spa`) to `<raw_html_dir>/<sha256(url)>.html`.
Default `None` preserves existing behavior exactly.

```python
# top of file
import hashlib
from pathlib import Path

# crawl(...) signature: add
#     raw_html_dir: Path | None = None,
# and pass it into _run_loop(...).

# _run_loop(...) signature: add
#     raw_html_dir: Path | None,

# inside _process, right after:  html = await _rescue_if_spa(url, html)
if raw_html_dir is not None and html:
    raw_html_dir.mkdir(parents=True, exist_ok=True)
    name = hashlib.sha256(url.encode()).hexdigest()[:16] + ".html"
    (raw_html_dir / name).write_text(html, encoding="utf-8")
```

The crawler keeps producing markdown for the knowledge base as before; this
only *additionally* persists raw HTML when a caller opts in. Knowledge base and
product graph stay separate stores (text vs. structured relations — see README).

## esg-side (done)

```bash
esg <raw_html_dir>      # mmaps each .html, builds graph.bin, runs queries
```

`build_from_files(&[PathBuf])` mmaps one file at a time; peak memory is ~one
page + the growing graph, independent of page count.

## Verified end-to-end

Three platforms (doni easy.co / cyberbiz / shopline) dropped into one dir →
single graph: 108 nodes / 64 edges across all three extraction tiers (L1
products[], L1 price_range, L2 ga-product), build 3.59ms, cypher 0.015ms.
