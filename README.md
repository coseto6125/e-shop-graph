# e-shop-graph (esg)

High-performance e-commerce knowledge graph built from schema.org structured
data. Extracts `Product`/`Offer`/`Brand`/`Review` from web pages' JSON-LD,
builds a zero-copy graph, serves single-digit-ms Cypher queries.

## Performance targets

- **Build**: ≤2s for a full crawl's worth of pages (network IO excluded).
- **Query**: single-digit milliseconds.

### Spike results (2 fixture pages, release build)

| stage | time |
|-------|------|
| build (extract + graph) | 1.46 ms |
| save (rkyv atomic write) | 0.04 ms |
| load (mmap zero-copy) | 0.008 ms |
| query (Product→Brand via CSR) | <0.001 ms |

Three orders of magnitude under budget. The real-world bottleneck is HTML
parsing, not graph build or serialization.

## Why this design (not Neo4j / KuzuDB / Python)

The single-digit-ms + 2s budget rules out server DBs (round-trip + JVM + query
plan) and Python (GIL + per-object allocation). The architecture is lifted from
ecp's `ZeroCopyGraph`:

- **string pool**: every string interned once; nodes hold `(offset, len)`, no
  per-node allocation.
- **CSR adjacency**: out-edges are contiguous slices — cache-friendly, one mmap
  brings the whole graph live.
- **rkyv zero-copy**: `load()` mmaps + `rkyv::access` WITHOUT deserializing.

## Integration with enoract

`enoract` already runs a mature, largely-Rust ingestion pipeline (never_primp
fetch, html→markdown, tantivy BM25, content-hash dedup, fingerprint atomic
swap). It has two gaps esg fills: it does **no** JSON-LD/schema.org parsing, and
it discards raw HTML after markdown extraction.

**Chosen integration (performance-optimal)**: esg is an independent Rust
workspace that also exposes a PyO3 binding (`esg-py`, planned). enoract calls
`esg.extract_jsonld(&html)` inline in `crawler.py::_process()` — at the one
point where raw HTML is still live — so the same HTML bytes fan out to two
consumers with **zero re-fetch and zero re-parse**:

```
crawler (never_primp → HTML)
  _process(html)
    ├─ html_to_clean_markdown → chunk → embed → PG → tantivy   (existing)
    └─ esg.extract_jsonld(&html) → schema.org → graph.bin       (new, inline)
```

graph.bin lands per-fingerprint, reusing enoract's existing dedup + atomic-swap
machinery, so incremental re-crawls update the graph naturally.

## Knowledge base vs graph — no duplicated content

The two stores are **complementary, not redundant**. They hold different kinds
of data and answer different questions:

| | knowledge base (tantivy + embedding) | graph (graph.bin) |
|---|---|---|
| holds | unstructured **text** (articles, descriptions, review bodies) | structured **relations** (product↔brand↔price↔rating) |
| answers | "what is this about", semantic similarity | "all products by Acme", "under 399 & 4★+" |
| query | vector / BM25 | Cypher traversal |

The split per page is by *nature of the data*, not "same data to both places":
structured JSON-LD fields (price, brand, rating, sku) go to the graph; prose
(description, review text, body paragraphs) goes to the knowledge base.

**Single source of truth for text**: graph nodes store queryable facts plus a
`chunk_id` pointer back to the knowledge base — never the full description text.
A GraphRAG query resolves an entity in the graph, then fetches its text from the
knowledge base by id. Text is stored once; the graph references it. This
prevents the same description being both embedded as a chunk and copied into a
node, which would otherwise double-recall at retrieval time.

Why both, not one: pure RAG can't answer "Acme headphones under 399 with 4★+"
(structured filter + relation traversal); a pure graph can't answer "which
headphones suit commuting noise" (semantic understanding of prose). Together
they are GraphRAG — graph for precise facts/relations/filters, vectors for fuzzy
semantics.

## Crates

- `esg-core` — zero-copy `Graph` (string pool + CSR), schema.org node/edge enums, rkyv store.
- `esg-extract` — HTML → JSON-LD → graph nodes/edges (rayon-parallel per page).
- `esg-cli` — `esg <dir>` vertical-slice spike + profiler.
- `esg-py` *(planned)* — PyO3 binding for inline calls from enoract.

## Status: vertical-slice spike

Proven: extract → build → rkyv save → mmap load → CSR query, all under budget.
Not yet built: Cypher engine (currently a hardcoded query), microdata/RDFa
fallback, Review/Person edges, RAG vector index, PyO3 binding.
