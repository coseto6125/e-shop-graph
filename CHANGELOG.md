# Changelog

## 0.8.3 — drop non-product nodes, capture multi-image galleries

`graph.bin` format is unchanged (VERSION 6). Extraction-side: re-crawl/re-build
existing graphs to apply. Adds a `List` Cypher value so `RETURN p.images`
projects an array.

### Added
- **Multi-image: `p.images`** — the full product gallery (deduped,
  order-preserving, `featured_image` first), alongside the existing primary
  `p.image`. Storefronts list ~9 photos/product under `images[]`; only the
  first reached the graph before. Written only when >1 image (single-image
  products keep just `p.image`).
- **Cypher list projection** — `RETURN p.images` now returns a real list
  (new `Value::List`), surfaced to Python as a `list`. Array-valued props
  previously projected to `Null`.

### Fixed
- **Non-product nodes are skipped.** An info / FAQ / blog page mislabelled
  `@type:Product` (real-world: easy.co `/blogs/news/常見問題`, name="DONI")
  reached the graph as a price-less, image-less Product, polluting carousel
  candidates. A multi-signal gate drops a node only when it has NO product
  signal at all — no price, no image, no sku/gtin/mpn, no `/products/` url, and
  (platform_json) no variant array. Any single signal keeps it, so a genuine
  product whose price is JS-rendered survives on its image/url alone. The page
  text still reaches the bm25/text lane.
- **Junk images filtered from galleries.** `extract_images` drops obvious
  chrome (logo/banner/icon/favicon/sprite/placeholder/theme-asset paths, and
  `.svg`) by URL path — a programmatic guard. The primary defence remains the
  source: only a product object's own `images[]` is read, where storefront
  chrome doesn't appear.

## 0.8.2 — price / image / url extraction across cyberbiz + shopline

`graph.bin` format is unchanged (VERSION 6, old files still load). This is an
extraction-side change: the new `price`/`image`/`url` props are written when a
page is (re-)ingested, so **existing graphs must be re-crawled/re-built to gain
them** — loading an old graph.bin works but its products keep the old (sparse)
props.

Validated end-to-end against real small/mid Taiwanese brand stores (茶籽堂,
綠藤生機, Cyberbiz, meepshop): products that previously reached the graph
name-only now carry price + image + canonical url.

### Fixed
- **Dropped the over-broad `"items"` key from the inline-product-array scan.**
  It false-positively matched cyberbiz's `product_labels.items` config array
  (`{"kind":"system","title":"特價標籤"}`) and shopline's `filter_tag` `items`
  array, minting label/tag nodes as Products and preempting the real products.
  A `kind`-field discriminator additionally guards the remaining keys against
  the same class of false positive.
- **JSON-LD products now surface an `image`.** `ingest_object` never called the
  shared image extractor, so JSON-LD-only products (shopline detail pages,
  meepshop) reached the graph thumbnail-less.
- **`extract_image` reads the schema.org singular `image:[url,…]` array shape**
  (previously only string / `{img_url|src|url}` object / plural `images[]`).
- **`AggregateOffer.lowPrice` is used when `Offer.price` is absent** — multi-
  variant stores (meepshop) expose a price band, not a scalar price.
- **A `ga-product` (shopline) page also folds in its JSON-LD subject.** On a
  detail page the `ga-product` attrs are recommendation-widget products
  (id/sku/title, no price/image) while the page subject lives ONLY in JSON-LD;
  both are now ingested (upsert-by-id merges overlaps). Listing pages, where
  `ga-product` is the rich source, are unaffected (no JSON-LD Product to fold).
- **`<link rel=canonical>` becomes `Product.url` when JSON-LD omits a url**,
  scoped to single-subject detail pages so recommendation products on the same
  page don't inherit it.

## 0.8.1 — `FUZZY` Cypher operator (CJK morpheme recall)

`graph.bin` format is unchanged (VERSION 6, old files still load) — this is a
query-side addition only, so existing graphs gain the operator with no rebuild.

### Added
- **`FUZZY` string-match operator: `WHERE p.name FUZZY '針織衫'`.** The needle is
  sliced into overlapping 2-grams and a node matches if its value contains ANY of
  them. This recovers CJK compounds the catalogue spells differently from the
  user — "針織衫" (which appears in no product name) matches "針織上衣"/"針織毛衣"
  on the shared morpheme "針織", where `CONTAINS '針織衫'` returns nothing.
  Cross-boundary grams ("織衫") match no product, so recall rises without noise.
  A needle shorter than 2 chars degrades to a plain `CONTAINS`. Operates on
  `char`s (multi-byte-safe), case-insensitive like the other string matchers.

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
