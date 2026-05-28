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
        Value::Object(_) => out.push(val),
        _ => {}
    }
}

/// Try extraction sources in priority order. The cheap byte-scan sources
/// (platform JSON, `ga-product`, `__NEXT_DATA__`) are attempted first without
/// touching the DOM or the price scale; the scale (a full visible-text scan)
/// is built only once a source actually yields products. The DOM-based
/// fallbacks (microdata, JSON-LD) share a single `Html::parse_document`.
fn extract_page(html: &str) -> (PageExtract, price::PriceScale) {
    let with_scale = |page| (page, price::PriceScale::from_html(html));

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
    (PageExtract::Empty, price::PriceScale::empty())
}

/// Fold one page's extraction result into the builder.
fn ingest_into(builder: &mut GraphBuilder, page: &PageExtract, scale: &price::PriceScale) {
    match page {
        PageExtract::Platform(products) => {
            for p in products {
                platform_json::ingest_product(builder, p, scale);
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
    let per_page: Vec<(PageExtract, price::PriceScale)> =
        pages.par_iter().map(|html| extract_page(html)).collect();
    let mut builder = GraphBuilder::new();
    for (page, scale) in &per_page {
        ingest_into(&mut builder, page, scale);
    }
    Ok(builder)
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
        let (page, scale) = extract_page(&html);
        ingest_into(&mut builder, &page, &scale);
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
/// any source carry the same `price_cents` / `currency` / `price_confident`
/// schema, so cross-source Cypher (`WHERE p.price_cents < 5000`) hits
/// every product regardless of how the page surfaced it.
fn ingest_object(b: &mut GraphBuilder, obj: &Value, scale: &price::PriceScale) {
    let ty = obj.get("@type").and_then(Value::as_str).unwrap_or("");
    if ty != "Product" {
        return;
    }
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
    // Stable id: prefer @id, then sku, then url, then name (last-resort).
    // `url` is added between sku and name because real-world JSON-LD often
    // omits @id/sku but always carries a canonical product URL — the same
    // field other extractors key on. Falling through to `name` instead
    // would collapse different products that happen to share a display
    // name into one node.
    let id = obj
        .get("@id")
        .or_else(|| obj.get("sku"))
        .or_else(|| obj.get("url"))
        .and_then(Value::as_str)
        .unwrap_or(name)
        .to_string();

    // Pull the offer's price field BEFORE the upsert so the Product node
    // ships with normalized price props. JSON-LD `offers` may be an Offer
    // object or a list of Offers; we take the first (typical retailer
    // layout — one offer per product).
    let offer_obj = obj
        .get("offers")
        .and_then(|o| if o.is_array() { o.get(0) } else { Some(o) });
    let offer_price = offer_obj.and_then(|o| o.get("price"));
    let props = platform_json::with_normalized_price(obj, offer_price, scale, None);
    let product_idx = b.upsert_node(NodeKind::Product, &id, name, &props);

    if let Some(brand) = obj.get("brand") {
        let bname = brand
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| brand.as_str())
            .unwrap_or("");
        if !bname.is_empty() {
            b.upsert_node(NodeKind::Brand, bname, bname, &brand.to_string());
            b.add_edge(product_idx, RelType::Brand, bname);
        }
    }

    if let Some(o) = offer_obj {
        let offer_id = format!("{id}#offer");
        let price = o.get("price").map(|v| v.to_string()).unwrap_or_default();
        b.upsert_node(NodeKind::Offer, &offer_id, &price, &o.to_string());
        b.add_edge(product_idx, RelType::Offers, &offer_id);
    }

    if let Some(rating) = obj.get("aggregateRating") {
        let rating_id = format!("{id}#rating");
        let rv = rating
            .get("ratingValue")
            .map(|v| v.to_string())
            .unwrap_or_default();
        b.upsert_node(
            NodeKind::AggregateRating,
            &rating_id,
            &rv,
            &rating.to_string(),
        );
        b.add_edge(product_idx, RelType::AggregateRating, &rating_id);
    }
}
