# enoract integration — raw HTML handoff

esg builds the product graph from **rendered** HTML. For Layer-3 stores (91app,
全聯, 7-11 —商品在 JS-fetched API), only enoract's crawler reaches the data,
because it runs chromium SPA fallback. The handoff is by **file system**: the
crawler writes one rendered HTML per page; esg streams them via
`build_from_files` (memory-bounded mmap, see `esg-extract/src/lib.rs`).

This avoids both OOM (a 10k-page crawl at 2MB/page would be ~20GB if held in
memory) and shipping GB of strings across a PyO3 boundary.

## enoract-side change (minimal, env-driven, opt-in)

Target: `enoract/console/knowledge/sources/web/crawler.py`. Apply with the
idempotent helper (verified against the real file; aborts if any anchor isn't
unique):

```bash
python docs/apply_enoract_handoff.py \
    enoract/console/knowledge/sources/web/crawler.py
```

It makes exactly TWO edits — an import and a persist block in `_process` (the
single funnel all `crawl*`/`_run_loop` paths flow through). It touches NO public
signature or call site, so there are no collisions across the several `crawl*`
functions. The handoff is driven by an env var:

```python
# inside _process, right after:  html = await _rescue_if_spa(url, html)
_esg_dir = os.environ.get("ESG_RAW_HTML_DIR")
if _esg_dir and html:
    _d = Path(_esg_dir); _d.mkdir(parents=True, exist_ok=True)
    _fname = hashlib.sha256(url.encode()).hexdigest()[:16] + ".html"
    (_d / _fname).write_text(html, encoding="utf-8")
```

Set `ESG_RAW_HTML_DIR` (to an org/bot-scoped path — tenancy is enoract's call)
and each rendered page lands there as one file; unset = exact original behavior.
The crawler keeps producing markdown for the knowledge base as before; this only
*additionally* persists raw HTML. Knowledge base and product graph stay separate
stores (text vs. structured relations — see README).

## Separation of concerns — esg is a pure engine, NOT multi-tenant

esg knows nothing about org/bot. It takes an input HTML dir and an output
`graph.bin` path, and does the graph work — it never interprets the path.

**Tenancy is enoract's job.** Org isolation happens naturally because enoract
passes a DIFFERENT output path per org/bot (it already owns `org_id`/`bot_slug`,
permissions, and the `{org_slug}/{bot_slug}/` layout). esg treats
`/data/acme/shop1/graph.bin` and `/data/other/shop2/graph.bin` as two unrelated
paths — physical isolation, zero cross-org leakage, yet esg stays org-agnostic.
Enumerating tenants and deleting a tenant's graph (CRUD) are likewise enoract's
responsibility; esg only builds/queries one graph at the path it's handed. Path-
traversal hardening on untrusted slugs belongs to enoract, which composes paths.

## esg-side (done)

```bash
esg <html_dir> [out_graph_path]   # build graph.bin at the caller-supplied path
```

`build_from_files(&[PathBuf])` mmaps one file at a time; peak memory is ~one
page + the growing graph, independent of page count. `out_graph_path` defaults
to `<html_dir>/graph.bin`; enoract supplies an org/bot-scoped path to isolate.

## Verified end-to-end

Three platforms (doni easy.co / cyberbiz / shopline) dropped into one dir →
single graph: 108 nodes / 64 edges across all three extraction tiers (L1
products[], L1 price_range, L2 ga-product), build 3.59ms, cypher 0.015ms.
