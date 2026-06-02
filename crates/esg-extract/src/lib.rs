//! Extraction layer: HTML bytes -> product graph nodes/edges.
//!
//! Multi-source auto-select chain (priority order):
//!   1. Platform product JSON (`"products":[...]` — doni/easy.co, cyberbiz)
//!   2. DOM attributes (`ga-product` — shopline)
//!   3. Next.js `__NEXT_DATA__` (SSR / post-render)
//!   4. schema.org microdata (`itemprop` — 91app SSR)
//!   5. schema.org JSON-LD (`@type: Product`)

pub mod dom_attr;
pub mod hypernova;
pub mod microdata;
pub mod next_data;
pub mod normalize;
pub mod platform_json;
pub mod price;

use anyhow::Result;
use esg_core::{GraphBuilder, NodeKind, RelType};
use rayon::prelude::*;
use scraper::{Html, Selector};
use serde_json::Value;

/// What a page yielded, after trying sources in priority order.
enum PageExtract {
    /// Layer 1: platform `"products":[...]` array (richest; doni/cyberbiz).
    Platform(Vec<Value>),
    /// Layer 2: products embedded in `ga-product='{}'` DOM attributes (shopline).
    /// The second field carries any `@type:Product` JSON-LD objects found on the
    /// SAME page: on a shopline DETAIL page the `ga-product` attrs are
    /// recommendation-widget products (id/sku/title, no price/image) while the
    /// page's true subject lives ONLY in JSON-LD — so we ingest both, and
    /// upsert-by-id merges where they overlap. Empty on listing pages (ga is the
    /// rich source there; nothing to augment).
    DomAttr(Vec<Value>, Vec<Value>),
    /// Next.js `__NEXT_DATA__` product-like arrays (SSR / post-render).
    NextData(Vec<Value>),
    /// schema.org microdata (itemprop) — 91app SSR product pages.
    Microdata(Vec<microdata::MicrodataProduct>),
    /// schema.org JSON-LD objects.
    JsonLd(Vec<Value>),
    /// Hypernova "SUPER LANDING" one-page store: product objects + the page
    /// canonical url (every product links to the single landing page).
    Hypernova(Vec<Value>, String),
    /// Nothing structured found.
    Empty,
}

/// One source page's extracted JSON-LD objects (already parsed).
pub struct PageRecords {
    pub objects: Vec<Value>,
}

static JSONLD_SEL: std::sync::LazyLock<Selector> =
    std::sync::LazyLock::new(|| Selector::parse(r#"script[type="application/ld+json"]"#).unwrap());

/// Pull every JSON-LD object out of one HTML document. Flattens `@graph`
/// containers and arrays into a flat object list.
pub fn extract_jsonld(html: &str) -> PageRecords {
    jsonld_from_dom(&Html::parse_document(html))
}

/// JSON-LD extraction from an already-parsed DOM, so `extract_page` can share
/// one `Html::parse_document` between the microdata and JSON-LD fallbacks.
fn jsonld_from_dom(doc: &Html) -> PageRecords {
    let mut objects = Vec::new();
    for el in doc.select(&JSONLD_SEL) {
        let text = el.text().collect::<String>();
        let Ok(val) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        collect_objects(val, &mut objects);
    }
    PageRecords { objects }
}

fn collect_objects(val: Value, out: &mut Vec<Value>) {
    match val {
        Value::Array(arr) => arr.into_iter().for_each(|v| collect_objects(v, out)),
        Value::Object(ref map) if map.contains_key("@graph") => {
            // @graph holds a node array; recurse into it.
            let mut obj = val;
            if let Some(graph) = obj.as_object_mut().and_then(|m| m.remove("@graph")) {
                collect_objects(graph, out);
            }
        }
        Value::Object(ref map) if map.contains_key("itemListElement") => {
            // ItemList (collection / category page): each itemListElement is a
            // ListItem wrapping the real entity under `item` (or is the entity
            // itself). Recurse so every listed Product becomes its own object,
            // instead of dropping the whole page as one non-Product node.
            let mut obj = val;
            if let Some(list) = obj
                .as_object_mut()
                .and_then(|m| m.remove("itemListElement"))
            {
                match list {
                    Value::Array(items) => items.into_iter().for_each(|it| {
                        let inner = it
                            .as_object()
                            .and_then(|m| m.get("item"))
                            .cloned()
                            .unwrap_or(it);
                        collect_objects(inner, out);
                    }),
                    other => collect_objects(other, out),
                }
            }
        }
        Value::Object(_) => out.push(val),
        _ => {}
    }
}

/// Try extraction sources in priority order. The cheap byte-scan sources
/// (platform JSON, `ga-product`, `__NEXT_DATA__`) are attempted first without
/// touching the DOM or the price scale; the scale (a full visible-text scan)
/// is built only once a source actually yields products. The DOM-based
/// fallbacks (microdata, JSON-LD) share a single `Html::parse_document`.
fn extract_page(html: &str) -> (PageExtract, price::PriceScale, PageMeta) {
    // Page origin (scheme://host) from og:url, shared across every extractor so
    // relative product/image URLs become absolute. Read straight from the HTML
    // (a meta tag) rather than parsing the DOM, so the JSON-only platform path
    // pays nothing extra. None when the page has no og:url — absolutize() then
    // leaves relative URLs as-is (better a relative URL than a wrong host).
    let origin = page_og_origin(html);
    // `canonical` is only consumed by the JSON-LD-bearing paths (DomAttr augment
    // + JsonLd), so it's computed inline in those two arms — the platform_json /
    // next_data / microdata arms never pay the page scan. `with_scale` defaults
    // canonical to None; the two arms that need it build PageMeta directly.
    let with_scale = |page| {
        (
            page,
            price::PriceScale::from_html(html),
            PageMeta {
                origin: origin.clone(),
                canonical: None,
            },
        )
    };

    // Source 0 (HIGHEST priority): clean JSON-LD Product / ProductGroup.
    // A page's `@type:Product` JSON-LD is the page subject's own structured
    // data — its name/price/image/sku, authored by the platform — so when it is
    // present and clean it must win over the lower sources that, on a detail
    // page, mis-fire on adjacent data (next_data.productList minting an empty
    // node, a BreadcrumbList microdata crumb, a GA category name). Gated behind
    // a cheap substring pre-check so a page WITHOUT json-ld (doni: 0 json-ld)
    // never pays the Html::parse_document tax and routes to platform_json /
    // microdata bit-identically to before.
    if html.contains("application/ld+json") {
        let ld = jsonld_from_dom(&Html::parse_document(html)).objects;
        let clean: Vec<Value> = ld.into_iter().filter(is_clean_product_ld).collect();
        if !clean.is_empty() {
            let canonical = page_canonical_url(html);
            return (
                PageExtract::JsonLd(clean),
                price::PriceScale::from_html(html),
                PageMeta { origin, canonical },
            );
        }
    }

    // Hypernova ("SUPER LANDING" one-page stores): the products array lives in a
    // keyed <script> whose body is an HTML COMMENT. Checked BEFORE platform_json
    // because platform_json's raw `"products":[` substring scan reaches INTO the
    // comment and would grab the array without the per-page canonical url that
    // ingest_hypernova stamps. Gated on the cheap `data-hypernova-key` substring
    // so non-Hypernova pages skip the scan.
    if html.contains("data-hypernova-key") {
        if let Some((products, canonical)) = hypernova::find_hypernova(html) {
            if !products.is_empty() {
                return (
                    PageExtract::Hypernova(products, canonical),
                    price::PriceScale::from_html(html),
                    PageMeta {
                        origin,
                        canonical: None,
                    },
                );
            }
        }
    }

    if let Some(products) = platform_json::find_products_array(html) {
        if !products.is_empty() {
            return with_scale(PageExtract::Platform(products));
        }
    }
    let ga = dom_attr::find_ga_products(html);
    if !ga.is_empty() {
        // A shopline DETAIL page's `ga-product` attrs are recommendation
        // products; the page subject (with price + image) is in the JSON-LD
        // Product block only. Carry those JSON-LD products so ingest folds both.
        let ld_products = jsonld_product_objects(&Html::parse_document(html));
        let canonical = (!ld_products.is_empty())
            .then(|| page_canonical_url(html))
            .flatten();
        return (
            PageExtract::DomAttr(ga, ld_products),
            price::PriceScale::from_html(html),
            PageMeta { origin, canonical },
        );
    }
    if let Some(nd) = next_data::find_next_data(html) {
        let products = next_data::collect_product_arrays(&nd);
        if !products.is_empty() {
            return with_scale(PageExtract::NextData(products));
        }
    }

    // DOM fallbacks: parse once, reuse for both microdata and JSON-LD.
    let doc = Html::parse_document(html);
    let md = microdata::extract_from_dom(&doc);
    if !md.is_empty() {
        return with_scale(PageExtract::Microdata(md));
    }
    let ld = jsonld_from_dom(&doc);
    if !ld.objects.is_empty() {
        let canonical = page_canonical_url(html);
        return (
            PageExtract::JsonLd(ld.objects),
            price::PriceScale::from_html(html),
            PageMeta { origin, canonical },
        );
    }
    (
        PageExtract::Empty,
        price::PriceScale::empty(),
        PageMeta {
            origin,
            canonical: None,
        },
    )
}

/// Page-level signals shared across extractors: the og:url `origin`
/// (scheme://host, for absolutizing relative URLs) and the `<link rel=canonical>`
/// product URL (surfaced as `Product.url` when a JSON-LD product omits its own).
struct PageMeta {
    origin: Option<String>,
    canonical: Option<String>,
}

/// JSON-LD objects on a parsed DOM that are `@type:Product` — the subset the
/// DomAttr augmentation re-ingests for a detail page's true subject.
fn jsonld_product_objects(doc: &Html) -> Vec<Value> {
    jsonld_from_dom(doc)
        .objects
        .into_iter()
        .filter(is_product_object)
        .collect()
}

/// `@type` is `Product` (string) or an array containing `"Product"`.
fn is_product_object(obj: &Value) -> bool {
    type_is(obj, "Product")
}

/// `@type` is `ProductGroup` (string) or an array containing `"ProductGroup"`.
/// A ProductGroup carries no price/image of its own; its sellable products live
/// under `hasVariant[]` (Shopify variant model), each a full `@type:Product`.
fn is_product_group(obj: &Value) -> bool {
    type_is(obj, "ProductGroup")
}

/// `@type` equals `want` as a bare string, or as a member of a `@type` array.
fn type_is(obj: &Value, want: &str) -> bool {
    match obj.get("@type") {
        Some(Value::String(s)) => s == want,
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some(want)),
        _ => false,
    }
}

/// A JSON-LD object that genuinely identifies a product: a Product (or a
/// ProductGroup with ≥1 clean variant) whose extracted prop map passes
/// `has_product_signal` (price / image / sku / gtin / mpn / `/products/` url).
/// A BreadcrumbList, a `WebPage`, or an empty `{@type:Product}` block fails
/// here, so the early JSON-LD-first arm only short-circuits when there is a
/// REAL product subject — otherwise the page falls through to the existing
/// source chain (microdata, etc.) exactly as before.
fn is_clean_product_ld(obj: &Value) -> bool {
    if is_product_group(obj) {
        return product_group_variants(obj).any(is_clean_product_ld);
    }
    if !is_product_object(obj) {
        return false;
    }
    // Build the SAME price/image signal view `ingest_object` would, cheaply:
    // a no-signal PriceScale (the reject path needs no visible-text scan — none
    // of the has_product_signal keys depend on the scale) plus the image probe.
    let offer_obj = obj
        .get("offers")
        .and_then(|o| if o.is_array() { o.get(0) } else { Some(o) });
    let offer_price = offer_obj.and_then(|o| o.get("price").or_else(|| o.get("lowPrice")));
    let mut pm = platform_json::normalized_price_map(obj, offer_price, &price::PriceScale::empty(), None);
    if let Some(img) = normalize::extract_image(obj) {
        pm.insert("image".into(), img.into());
    }
    has_product_signal(&pm)
}

/// A ProductGroup's `hasVariant` children as a slice iterator — handles both the
/// array form (the common case) and a single-object form. Empty when absent.
fn product_group_variants(obj: &Value) -> impl Iterator<Item = &Value> {
    match obj.get("hasVariant") {
        Some(Value::Array(arr)) => arr.as_slice(),
        Some(one) => std::slice::from_ref(one),
        None => &[],
    }
    .iter()
}

/// A ProductGroup's stable group identity, for stamping its variants. Prefers
/// `productGroupID` (schema.org's explicit grouping key) then `@id` then the
/// group `url` — the same identity-priority spirit as the per-Product id, but a
/// group rarely carries a numeric id so no numeric arm is needed. None when the
/// group declares no identity (then variants stay ungrouped rather than sharing
/// a bogus key).
fn product_group_id(obj: &Value) -> Option<String> {
    ["productGroupID", "@id", "url"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Extract the `<link rel="canonical" href="…">` product URL — a cheap
/// substring scan (no DOM build for the JSON-only path). None when absent.
/// This is the page's own declared canonical, used as `Product.url` when the
/// structured data omits a url.
fn page_canonical_url(html: &str) -> Option<String> {
    // Find a <link …rel="canonical"…> tag, then its href. rel and href can
    // appear in either order, so locate the tag first then scan within it.
    let mut search = html;
    while let Some(rel_pos) = search
        .find("rel=\"canonical\"")
        .or_else(|| search.find("rel='canonical'"))
    {
        // Bound the tag: back to the nearest '<', forward to the next '>'.
        let tag_start = search[..rel_pos].rfind('<').unwrap_or(0);
        let tag_end = search[rel_pos..]
            .find('>')
            .map(|e| rel_pos + e)
            .unwrap_or(search.len());
        let tag = &search[tag_start..tag_end];
        if let Some(href_pos) = tag.find("href=") {
            let after = &tag[href_pos + "href=".len()..];
            if let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let val = &after[1..];
                if let Some(end) = val.find(quote) {
                    let url = val[..end].trim();
                    if url.starts_with("http") {
                        return Some(url.to_string());
                    }
                }
            }
        }
        search = &search[tag_end..];
    }
    None
}

/// Extract the page origin (`scheme://host`) from the `og:url` meta tag without
/// parsing the DOM — a cheap substring scan, since the JSON-first path never
/// builds a `Html` document. Returns None if there's no og:url or it's relative.
fn page_og_origin(html: &str) -> Option<String> {
    // Find the og:url meta, then the `content="…"` that follows it on the tag.
    let tag_start = html.find("og:url")?;
    let rest = &html[tag_start..];
    let content_pos = rest.find("content=")?;
    let after = &rest[content_pos + "content=".len()..];
    let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &after[1..];
    let end = value.find(quote)?;
    normalize::url_origin(value[..end].trim())
}

/// Fold one page's extraction result into the builder. `origin` (the page's
/// og:url scheme://host) absolutizes relative product/image URLs uniformly
/// across every extractor.
fn ingest_into(
    builder: &mut GraphBuilder,
    page: &PageExtract,
    scale: &price::PriceScale,
    meta: &PageMeta,
) {
    let origin = meta.origin.as_deref();
    match page {
        PageExtract::Platform(products) => {
            for p in products {
                platform_json::ingest_product(builder, p, scale, origin);
            }
        }
        PageExtract::DomAttr(products, ld_products) => {
            for p in products {
                dom_attr::ingest_ga_product(builder, p, scale);
            }
            // Detail-page subject: ga carried only recommendation products, so
            // fold the JSON-LD Product(s) too (price + image live there). Stamp
            // the page canonical only when there's a single JSON-LD subject — a
            // detail page — so a multi-product JSON-LD block isn't mis-attributed
            // one shared url.
            let canonical = (ld_products.len() == 1)
                .then_some(meta.canonical.as_deref())
                .flatten();
            for obj in ld_products {
                ingest_object(builder, obj, scale, canonical, None);
            }
        }
        PageExtract::NextData(products) => {
            for p in products {
                next_data::ingest_next_product(builder, p, scale);
            }
        }
        PageExtract::Microdata(products) => {
            microdata::ingest_microdata(builder, products, scale);
        }
        PageExtract::JsonLd(objects) => {
            // Stamp the canonical only for a single-subject (detail) page.
            let canonical = (objects.iter().filter(|o| is_product_object(o)).count() == 1)
                .then_some(meta.canonical.as_deref())
                .flatten();
            for obj in objects {
                ingest_object(builder, obj, scale, canonical, None);
            }
        }
        PageExtract::Hypernova(products, canonical) => {
            hypernova::ingest_hypernova(builder, products, scale, canonical, origin);
        }
        PageExtract::Empty => {}
    }
}

/// Parse in-memory pages in parallel, fold into one graph. Suited to small
/// batches where holding all HTML in memory is fine. Borrows the pages — it
/// only reads them, so callers keep ownership.
pub fn build_from_pages(pages: &[String]) -> Result<GraphBuilder> {
    let mut builder = GraphBuilder::new();
    ingest_pages_into(&mut builder, pages);
    Ok(builder)
}

/// Extract `pages` and fold them into an EXISTING builder — the extraction half
/// of incremental rebuild. A builder rehydrated via `GraphBuilder::from_graph`
/// can take a handful of re-crawled pages here; each product `upsert_node`s,
/// so a changed price overwrites in place rather than duplicating. Extraction
/// is parallel (rayon); the ingest fold is serial because the builder is one
/// shared mutable structure.
pub fn ingest_pages_into(builder: &mut GraphBuilder, pages: &[String]) {
    let per_page: Vec<(PageExtract, price::PriceScale, PageMeta)> = pages
        .par_iter()
        .enumerate()
        .map(|(i, html)| extract_page_isolated(html, i))
        .collect();
    for (page, scale, meta) in &per_page {
        ingest_into(builder, page, scale, meta);
    }
}

/// `extract_page` with a panic firewall: a single poison page (a malformed JSON
/// depth, a future serde stack overflow, a substring slice on a non-char
/// boundary) degrades to an empty extract instead of unwinding out of the rayon
/// `collect()` and aborting the ENTIRE build (which discarded every good page —
/// the 64-byte-graph failure mode). `&str` is unwind-safe; the closure captures
/// nothing mutable, so `AssertUnwindSafe` is sound. The caught panic is logged
/// with the page index so a systematic failure stays visible in build logs.
fn extract_page_isolated(html: &str, idx: usize) -> (PageExtract, price::PriceScale, PageMeta) {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract_page(html))).unwrap_or_else(
        |_| {
            eprintln!("esg: extract panicked on page index {idx}; skipping");
            (
                PageExtract::Empty,
                price::PriceScale::empty(),
                PageMeta {
                    origin: None,
                    canonical: None,
                },
            )
        },
    )
}

/// Memory-bounded build: mmap each HTML file in `paths` one at a time, extract,
/// fold into the graph, then drop the mapping before the next file. Peak memory
/// is ~one page + the growing graph — INDEPENDENT of page count, so a 10k-page
/// crawl with 2MB pages never holds 20GB. This is the enoract handoff path:
/// the crawler writes one rendered HTML per file, esg streams them here.
pub fn build_from_files(paths: &[std::path::PathBuf]) -> Result<GraphBuilder> {
    use std::fs::File;
    let mut builder = GraphBuilder::new();
    for path in paths {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                tracing_warn(path, &e);
                continue;
            }
        };
        // SAFETY: file is read-only; mapping is dropped at end of iteration.
        let mmap = match unsafe { memmap2::Mmap::map(&file) } {
            Ok(m) => m,
            Err(e) => {
                tracing_warn(path, &e);
                continue;
            }
        };
        // HTML may not be valid UTF-8 in the strict sense; lossy is fine for
        // extraction (we only read ASCII-structured JSON/attrs + text).
        let html = String::from_utf8_lossy(&mmap);
        // Same panic firewall as the parallel path: a single poison file is
        // skipped (logged with its path), never fatal to the whole crawl.
        let (page, scale, meta) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract_page(&html)))
                .unwrap_or_else(|_| {
                    eprintln!("esg: extract panicked on {path:?}; skipping");
                    (
                        PageExtract::Empty,
                        price::PriceScale::empty(),
                        PageMeta {
                            origin: None,
                            canonical: None,
                        },
                    )
                });
        ingest_into(&mut builder, &page, &scale, &meta);
        // mmap dropped here → page memory released before the next file.
    }
    Ok(builder)
}

fn tracing_warn(path: &std::path::Path, e: &std::io::Error) {
    eprintln!("esg: skip {path:?}: {e}");
}

/// True when a built props map carries at least one signal that it's a real
/// product — a price, an image, a product identity key (sku/gtin/mpn), or a
/// `/products/` URL path. An info / FAQ / blog page mislabelled `@type:Product`
/// has none of these. Any single signal keeps the node, so a genuine product
/// whose price is JS-rendered (absent from the static HTML) survives on its
/// image or url alone — the gate excludes the empty, never the merely partial.
pub(crate) fn has_product_signal(pm: &serde_json::Map<String, Value>) -> bool {
    let nonempty = |k: &str| {
        pm.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    let has_num = |k: &str| pm.get(k).is_some_and(|v| !v.is_null());
    has_num("price_cents")
        || nonempty("price")
        || nonempty("image")
        || nonempty("sku")
        || nonempty("gtin")
        || nonempty("gtin13")
        || nonempty("mpn")
        || pm
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|u| u.contains("/products/") || u.contains("/product/"))
}

/// Map a single schema.org object onto graph nodes + edges. Currently handles
/// the Product-centric core (Product/Offer/Brand/AggregateRating).
///
/// Threading `scale` here keeps JSON-LD on the same normalization path as
/// platform_json / dom_attr / next_data / microdata — Product nodes from
/// any source carry the same `price` (string, whole units) / `currency` /
/// `price_confident` schema, so cross-source Cypher
/// (`WHERE p.price = "4200"` or string-prefix matches) hits every product
/// regardless of how the page surfaced it.
fn ingest_object(
    b: &mut GraphBuilder,
    obj: &Value,
    scale: &price::PriceScale,
    page_url: Option<&str>,
    group_id: Option<&str>,
) {
    // ProductGroup (Shopify variant model): the group carries no price/image of
    // its own — its sellable products live under `hasVariant[]`, each a full
    // `@type:Product` with its own sku/image/offers.price. Emit one Product per
    // variant (distinct sku/@id ⇒ distinct nodes; per-variant price/stock/image
    // would be lost by merging). A parent Product node would fail the signal
    // gate and orphan the variants, so per-variant Product is the only shape
    // that survives the gate. The group's identity (`@id`/`productGroupID`) is
    // threaded into each variant so downstream retrieval can collapse a multi-
    // colour product to one carousel card via `COALESCE(product_group_id, id)` —
    // a store with no ProductGroup (doni, plain detail pages) leaves it None and
    // every product is its own group (the coalesce is a no-op there).
    if is_product_group(obj) {
        let gid = product_group_id(obj);
        for v in product_group_variants(obj) {
            ingest_object(b, v, scale, page_url, gid.as_deref());
        }
        return;
    }
    if !is_product_object(obj) {
        return;
    }
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
    // Identity priority: product id (`productId`/`id`, string OR numeric) →
    // @id → sku → url → name. The store product id is the cross-view stable key
    // (a listing card / detail page elsewhere carry the same one), so it wins
    // when present — and it may be a JSON number, which a string-only read would
    // miss. `url` stays ahead of `name` because real-world JSON-LD often omits
    // every id but always carries a canonical product URL; falling to `name`
    // would collapse distinct products that share a display name.
    let id = obj
        .get("productId")
        .or_else(|| obj.get("id"))
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
        .or_else(|| {
            obj.get("@id")
                .or_else(|| obj.get("sku"))
                .or_else(|| obj.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| name.to_string());

    // Pull the offer's price field BEFORE the upsert so the Product node
    // ships with normalized price props. JSON-LD `offers` may be an Offer
    // object or a list of Offers; we take the first for the Product-level
    // price (typical retailer layout — one offer per product), but every
    // offer in an array becomes its own Offer node below.
    let offer_obj = obj
        .get("offers")
        .and_then(|o| if o.is_array() { o.get(0) } else { Some(o) });
    // `Offer` carries a scalar `price`; `AggregateOffer` (a price band over
    // variants — meepshop and other multi-variant stores) carries no `price`,
    // only `lowPrice`/`highPrice`. Fall back to `lowPrice` so the Product node
    // gets the band's floor as its price rather than nothing.
    let offer_price = offer_obj.and_then(|o| o.get("price").or_else(|| o.get("lowPrice")));
    // Start from the normalized-price Map directly (no serialize→parse round
    // trip) and inline scalar props onto the Product so single-node Cypher
    // works without a hop: offer availability/condition/validity/seller, the
    // GTIN/MPN/SKU identity keys, and the aggregateRating value/count. These
    // are JSON-LD (and platform-where-present) signals; absent fields omitted.
    let mut pm = platform_json::normalized_price_map(obj, offer_price, scale, None);
    if let Some(o) = offer_obj {
        for (src, dst) in [
            ("availability", "availability"),
            ("itemCondition", "item_condition"),
            ("priceValidUntil", "price_valid_until"),
        ] {
            if let Some(v) = o.get(src).and_then(Value::as_str) {
                pm.insert(dst.into(), schema_enum_tail(v).into());
            }
        }
        if let Some(s) = o
            .get("seller")
            .and_then(|s| s.get("name"))
            .and_then(Value::as_str)
        {
            pm.insert("seller_name".into(), s.into());
        }
    }
    for (src, dst) in [
        ("gtin13", "gtin13"),
        ("gtin", "gtin"),
        ("mpn", "mpn"),
        ("sku", "sku"),
    ] {
        if let Some(v) = obj.get(src).and_then(Value::as_str) {
            pm.insert(dst.into(), v.into());
        }
    }
    if let Some(rating) = obj.get("aggregateRating") {
        if let Some(rv) = rating.get("ratingValue") {
            pm.insert("rating_value".into(), rv.clone());
        }
        if let Some(c) = rating
            .get("reviewCount")
            .or_else(|| rating.get("ratingCount"))
        {
            pm.insert("review_count".into(), c.clone());
        }
    }
    // Surface a single `image` from the JSON-LD `image` field (a CDN string, an
    // `{img_url|src|url}` object, or — the schema.org norm — an array of URL
    // strings), mirroring what platform_json::ingest_product does for the inline
    // JSON path. Without this a JSON-LD-only product (shopline detail pages,
    // meepshop) reached the graph thumbnail-less. JSON-LD images are absolute CDN
    // URLs, so no page origin is threaded in here.
    if let Some(img) = normalize::extract_image(obj) {
        pm.insert("image".into(), normalize::absolutize_url(&img, None).into());
    }
    // Full gallery alongside the primary `image` (same as platform_json), only
    // when there's more than one photo. JSON-LD images are absolute CDN URLs.
    let images = normalize::extract_images(obj);
    if images.len() > 1 {
        pm.insert(
            "images".into(),
            Value::Array(images.into_iter().map(Value::String).collect()),
        );
    }
    // A ProductGroup variant carries no top-level `url`; its link lives on the
    // offer (`offers.url` — Shopify's per-variant product URL). Fall back to it
    // before the page canonical so each variant card is independently clickable
    // (the canonical, gated to single-subject pages, never reaches a variant).
    if let Some(offer_url) = offer_obj
        .and_then(|o| o.get("url"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        pm.entry("url".to_string())
            .or_insert_with(|| offer_url.to_string().into());
    }
    // When the JSON-LD product carries no `url` of its own, fall back to the
    // page's `<link rel=canonical>` — a real product URL (a LINE carousel uri)
    // instead of the opaque sku/@id the identity logic falls back to. Only the
    // page subject reaches here with a canonical (caller gates it to single-
    // product detail pages), so a recommendation product never inherits it.
    if let Some(url) = page_url {
        pm.entry("url".to_string())
            .or_insert_with(|| url.to_string().into());
    }
    // Multi-signal gate: a node with NO product signal at all — no price, no
    // image, no /products/ url, no sku/handle — is an info/FAQ/blog page that a
    // `@type:Product` block (or a fallback-to-name id) mislabelled, not a real
    // product. Skipping it keeps the carousel candidate set clean while the
    // page's text still reaches the bm25/text lane. Any ONE signal present is
    // enough to keep it (a real product whose price is JS-rendered still has an
    // image / a /products/ url), so this never drops a genuine product.
    if !has_product_signal(&pm) {
        return;
    }
    // Variant grouping: stamp the owning ProductGroup id so retrieval can roll a
    // multi-variant product up to one carousel card via
    // `COALESCE(product_group_id, id)` — distinct ids when ungrouped, a shared
    // id within a group. None when the product has no group (the common case:
    // doni / plain detail pages carry no prop, so the coalesce is a no-op and
    // every product stays its own card). A prop, not an `IsVariantOf` edge: the
    // edge would need a ProductGroup target node (which has no price/image of
    // its own and would fail the signal gate), and the prop alone is what the
    // group-by collapse reads — the edge would be dropped as dangling anyway.
    if let Some(gid) = group_id {
        pm.insert("product_group_id".into(), gid.into());
    }
    let props = Value::Object(pm).to_string();
    let product_idx = b.upsert_node(NodeKind::Product, &id, name, &props);

    // brand = marketing label, manufacturer = legal maker (distinct nodes;
    // they often differ in electronics / regulated goods).
    ingest_named_entity(
        b,
        obj,
        "brand",
        NodeKind::Brand,
        RelType::Brand,
        product_idx,
    );
    ingest_named_entity(
        b,
        obj,
        "manufacturer",
        NodeKind::Organization,
        RelType::Manufacturer,
        product_idx,
    );
    ingest_offers(b, obj, &id, product_idx);
    ingest_aggregate_rating(b, obj, &id, product_idx);
    ingest_reviews(b, obj, &id, product_idx);
}

/// schema.org enum-valued props are URLs (`https://schema.org/InStock`); keep
/// only the trailing token so `WHERE p.availability = "InStock"` reads cleanly.
fn schema_enum_tail(v: &str) -> &str {
    v.rsplit('/').next().unwrap_or(v)
}

/// Wire a named sub-entity (`brand` → Brand, `manufacturer` → Organization)
/// into its own node + edge. The field value may be an object (`{name}`) or a
/// bare string; the node is keyed and named by that name.
fn ingest_named_entity(
    b: &mut GraphBuilder,
    obj: &Value,
    field: &str,
    kind: NodeKind,
    rel: RelType,
    product_idx: u32,
) {
    if let Some(entity) = obj.get(field) {
        let name = entity
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| entity.as_str())
            .unwrap_or("");
        if !name.is_empty() {
            b.upsert_node(kind, name, name, &entity.to_string());
            b.add_edge(product_idx, rel, name);
        }
    }
}

/// `Product.offers` → one Offer node per offer (an array carries multiple
/// seller/price offers), each edged Product-[:Offers]->Offer. A single offer
/// object keeps the bare `{id}#offer` id so existing graphs don't churn; array
/// elements get `{id}#offer{n}`.
fn ingest_offers(b: &mut GraphBuilder, obj: &Value, id: &str, product_idx: u32) {
    let Some(offers) = obj.get("offers") else {
        return;
    };
    let emit = |b: &mut GraphBuilder, o: &Value, offer_id: &str| {
        let price = o.get("price").map(|v| v.to_string()).unwrap_or_default();
        b.upsert_node(NodeKind::Offer, offer_id, &price, &o.to_string());
        b.add_edge(product_idx, RelType::Offers, offer_id);
    };
    match offers.as_array() {
        Some(arr) => {
            for (n, o) in arr.iter().enumerate() {
                emit(b, o, &format!("{id}#offer{n}"));
            }
        }
        None => emit(b, offers, &format!("{id}#offer")),
    }
}

/// `Product.aggregateRating` → AggregateRating node (rating_value / review_count
/// hoisted to queryable scalars) + edge.
fn ingest_aggregate_rating(b: &mut GraphBuilder, obj: &Value, id: &str, product_idx: u32) {
    let Some(rating) = obj.get("aggregateRating") else {
        return;
    };
    let rating_id = format!("{id}#rating");
    let rv = rating
        .get("ratingValue")
        .map(|v| v.to_string())
        .unwrap_or_default();
    let mut rm = rating.as_object().cloned().unwrap_or_default();
    if let Some(c) = rating
        .get("reviewCount")
        .or_else(|| rating.get("ratingCount"))
    {
        rm.insert("review_count".into(), c.clone());
    }
    if let Some(v) = rating.get("ratingValue") {
        rm.insert("rating_value".into(), v.clone());
    }
    b.upsert_node(
        NodeKind::AggregateRating,
        &rating_id,
        &rv,
        &Value::Object(rm).to_string(),
    );
    b.add_edge(product_idx, RelType::AggregateRating, &rating_id);
}

/// `Product.review` → Review nodes (rating_value / date hoisted) + edge, and
/// each review's `author` → Person node + Author edge. `review` may be a single
/// object or an array.
fn ingest_reviews(b: &mut GraphBuilder, obj: &Value, id: &str, product_idx: u32) {
    // Borrow the reviews in place — `review` is an array or a single object;
    // `from_ref` views the single object as a 1-element slice with no clone.
    let reviews: &[Value] = match obj.get("review") {
        Some(Value::Array(arr)) => arr,
        Some(one) => std::slice::from_ref(one),
        None => return,
    };
    for (i, rv) in reviews.iter().enumerate() {
        let rid = format!("{id}#review{i}");
        let body = rv.get("reviewBody").and_then(Value::as_str).unwrap_or("");
        let mut rm = rv.as_object().cloned().unwrap_or_default();
        if let Some(r) = rv.get("reviewRating").and_then(|x| x.get("ratingValue")) {
            rm.insert("rating_value".into(), r.clone());
        }
        if let Some(d) = rv.get("datePublished") {
            rm.insert("date".into(), d.clone());
        }
        let review_idx =
            b.upsert_node(NodeKind::Review, &rid, body, &Value::Object(rm).to_string());
        b.add_edge(product_idx, RelType::Review, &rid);

        let author = rv.get("author");
        let aname =
            author.and_then(|a| a.get("name").and_then(Value::as_str).or_else(|| a.as_str()));
        if let Some(an) = aname.filter(|s| !s.is_empty()) {
            b.upsert_node(
                NodeKind::Person,
                an,
                an,
                &author.map(|a| a.to_string()).unwrap_or_default(),
            );
            b.add_edge(review_idx, RelType::Author, an);
        }
    }
}
