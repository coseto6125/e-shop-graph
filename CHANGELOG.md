# Changelog

## 0.8.0 — product-id node identity (dedup detail/listing into one node)

Rebuild graphs to benefit — `graph.bin` format is unchanged (VERSION 6, old
files still load), but node identity changed, so duplicate Product nodes only
collapse on a fresh build.

### Fixed
- One product reached the graph as **two Product nodes** when its detail page
  and a collection-listing card were extracted by different paths keying on
  different ids (the detail page on its URL, the listing card on the numeric
  product id). The description (detail page) and the price/image (listing card)
  landed on separate nodes, so a consumer that picked the wrong node saw a
  product with no description. On the doni catalogue this hit 559 of 693
  products.

### Changed
- **Unified Product identity priority across every extractor: store product id
  → url/handle → name.** The product id is the cross-view stable key, so a
  detail page and a listing card for the same product now share one node id.
  Ids are read whether they're a JSON string or number (a product id is
  frequently numeric). Stores that expose no id fall back to the url (then
  name), so identity is never worse than before.
- microdata now reads the page's store product id from `data-addtocart='{"id":
  N,…}'` / `data-product-id` on single-product detail pages, so a detail page
  shares the listing card's id.
- `upsert_node` now **merges props field-wise (non-empty wins)** instead of a
  last-write-wins overwrite. Re-stating a product under one id from several
  sources accumulates each source's fields (description from one, image/price
  from another) rather than letting a later, sparser statement erase an earlier
  one. A re-crawl still updates a changed price (a new non-empty value wins).

### Performance
- Build-time id/intern maps now hash with xxh3 (already a dependency for the
  graph fingerprint) instead of SipHash — faster lookups/inserts on the short
  string keys hammered during a build, with no DoS concern on trusted crawl data.

## 0.6.0 — crash hardening, query perf, schema.org expansion

On-disk `graph.bin` format VERSION 2 → 3 (rebuild graphs; old files are
rejected at load with a clear version-mismatch error).

### Fixed (crash)
- Cypher lexer panicked on any non-ASCII byte in identifier position (a CJK
  property name like `RETURN p.名稱`, emoji, accented letter). Now lexes
  Unicode identifiers without panicking.
- `iso_code_after` sliced a 16-byte window on a non-char boundary, panicking on
  crafted/multibyte HTML. Now scans the window at the byte level.
- `PriceScale::verdict` panicked on `Decimal` multiply overflow for a peer near
  `Decimal::MAX`; now uses checked arithmetic and skips corroboration on overflow.
- `LoadedGraph::open` now validates `graph.bin` invariants (MAGIC/VERSION, CSR
  offset length/monotonicity, edge-target and string-pool bounds) and returns a
  recoverable error instead of letting a corrupt/foreign file panic the executor.

### Fixed (correctness)
- `whole_units` mis-parsed single-decimal price tokens (`5.5` → `55`); the
  fractional part (1–2 digits) is now dropped, 3-digit groups kept as thousands.
- Numeric-looking identifier strings no longer lose identity: a leading-zero SKU
  `"0123"` stays a string instead of coercing to `Int(123)`.
- Cross-type `<>` is now true (two non-null incomparable values are not equal),
  and any comparison involving null is never true (Cypher 3-valued logic).
- `iso_code_after` reads lower-case ISO currency codes (`"currency":"usd"`).

### Performance
- Cypher query: per-query caching of parsed `props` JSON, pool strings borrowed
  (not copied) from the mmap, and decorate-sort for ORDER BY. ~2.6× faster on a
  5000-node WHERE(3 refs)+ORDER BY query (6.44 → 2.45 ms).

### Added (schema.org)
- Review / Person / Organization nodes and the Manufacturer / Review / Author
  edges are now emitted from JSON-LD (previously declared but never populated).
- Offer scalars (availability, item_condition, price_valid_until, seller_name)
  and GTIN/MPN/SKU identity keys inlined onto Product for single-node queries.
- AggregateRating value/count hoisted to queryable scalars and mirrored on Product.
- `offers` arrays emit one Offer node per element; `ItemList` collection pages
  expand into one Product per item; `@type` arrays containing `Product` accepted.
- microdata reader groups by Product itemscope, so a listing page yields one
  product per card instead of one cross-wired product.
- New `RelType::BroaderCategory` for Category breadcrumb parent chains.
