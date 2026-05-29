//! Extraction layer: HTML bytes -> product graph nodes/edges.
//!
//! Multi-source auto-select chain (priority order):
//!   1. Platform product JSON (`"products":[...]` — doni/easy.co, cyberbiz)
//!   2. DOM attributes (`ga-product` — shopline)
//!   3. Next.js `__NEXT_DATA__` (SSR / post-render)
//!   4. schema.org microdata (`itemprop` — 91app SSR)
//!   5. schema.org JSON-LD (`@type: Product`)

pub mod dom_attr;
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
    DomAttr(Vec<Value>),
    /// Next.js `__NEXT_DATA__` product-like arrays (SSR / post-render).
    NextData(Vec<Value>),
    /// schema.org microdata (itemprop) — 91app SSR product pages.
    Microdata(Vec<microdata::MicrodataProduct>),
    /// schema.org JSON-LD objects.
    JsonLd(Vec<Value>),
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
fn extract_page(html: &str) -> (PageExtract, price::PriceScale, Option<String>) {
    // Page origin (scheme://host) from og:url, shared across every extractor so
    // relative product/image URLs become absolute. Read straight from the HTML
    // (a meta tag) rather than parsing the DOM, so the JSON-only platform path
    // pays nothing extra. None when the page has no og:url — absolutize() then
    // leaves relative URLs as-is (better a relative URL than a wrong host).
    let origin = page_og_origin(html);
    let with_scale = |page| (page, price::PriceScale::from_html(html), origin.clone());

    if let Some(products) = platform_json::find_products_array(html) {
        if !products.is_empty() {
            return with_scale(PageExtract::Platform(products));
        }
    }
    let ga = dom_attr::find_ga_products(html);
    if !ga.is_empty() {
        return with_scale(PageExtract::DomAttr(ga));
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
        return with_scale(PageExtract::JsonLd(ld.objects));
    }
    (PageExtract::Empty, price::PriceScale::empty(), origin)
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
    origin: Option<&str>,
) {
    match page {
        PageExtract::Platform(products) => {
            for p in products {
                platform_json::ingest_product(builder, p, scale, origin);
            }
        }
        PageExtract::DomAttr(products) => {
            for p in products {
                dom_attr::ingest_ga_product(builder, p, scale);
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
            for obj in objects {
                ingest_object(builder, obj, scale);
            }
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
    let per_page: Vec<(PageExtract, price::PriceScale, Option<String>)> =
        pages.par_iter().map(|html| extract_page(html)).collect();
    for (page, scale, origin) in &per_page {
        ingest_into(builder, page, scale, origin.as_deref());
    }
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
        let (page, scale, origin) = extract_page(&html);
        ingest_into(&mut builder, &page, &scale, origin.as_deref());
        // mmap dropped here → page memory released before the next file.
    }
    Ok(builder)
}

fn tracing_warn(path: &std::path::Path, e: &std::io::Error) {
    eprintln!("esg: skip {path:?}: {e}");
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
fn ingest_object(b: &mut GraphBuilder, obj: &Value, scale: &price::PriceScale) {
    // `@type` is usually a string but schema.org permits an array of types
    // (e.g. `["Product","IndividualProduct"]`); accept either as long as
    // "Product" is present.
    let is_product = match obj.get("@type") {
        Some(Value::String(s)) => s == "Product",
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some("Product")),
        _ => false,
    };
    if !is_product {
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
    let offer_price = offer_obj.and_then(|o| o.get("price"));
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
