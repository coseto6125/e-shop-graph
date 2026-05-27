//! Extraction layer: HTML bytes -> schema.org records -> graph nodes/edges.
//!
//! Strategy: prefer `<script type="application/ld+json">` (the SEO-standard
//! embedding every major e-commerce site ships). JSON-LD is clean structured
//! data — no DOM heuristics needed for the happy path. microdata/RDFa fallback
//! is a future addition (see README risks).

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

/// Pull every JSON-LD object out of one HTML document. Flattens `@graph`
/// containers and arrays into a flat object list.
pub fn extract_jsonld(html: &str) -> PageRecords {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).unwrap();
    let mut objects = Vec::new();
    for el in doc.select(&sel) {
        let text = el.text().collect::<String>();
        let Ok(val) = serde_json::from_str::<Value>(&text) else { continue };
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

/// Try extraction sources in priority order: platform product JSON first
/// (richest), then schema.org JSON-LD. DOM/microdata fallback is future work.
/// Each page carries its own `PriceScale` (visible prices) for unit inference.
fn extract_page(html: &str) -> (PageExtract, price::PriceScale) {
    let scale = price::PriceScale::from_html(html);
    if let Some(products) = platform_json::find_products_array(html) {
        if !products.is_empty() {
            return (PageExtract::Platform(products), scale);
        }
    }
    let ga = dom_attr::find_ga_products(html);
    if !ga.is_empty() {
        return (PageExtract::DomAttr(ga), scale);
    }
    if let Some(nd) = next_data::find_next_data(html) {
        let products = next_data::collect_product_arrays(&nd);
        if !products.is_empty() {
            return (PageExtract::NextData(products), scale);
        }
    }
    let md = microdata::extract_microdata_products(html);
    if !md.is_empty() {
        return (PageExtract::Microdata(md), scale);
    }
    let ld = extract_jsonld(html);
    if !ld.objects.is_empty() {
        return (PageExtract::JsonLd(ld.objects), scale);
    }
    (PageExtract::Empty, scale)
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
                next_data::ingest_next_product(builder, p);
            }
        }
        PageExtract::Microdata(products) => {
            microdata::ingest_microdata(builder, products);
        }
        PageExtract::JsonLd(objects) => {
            for obj in objects {
                ingest_object(builder, obj);
            }
        }
        PageExtract::Empty => {}
    }
}

/// Parse in-memory pages in parallel, fold into one graph. Suited to small
/// batches where holding all HTML in memory is fine.
pub fn build_from_pages(pages: Vec<String>) -> Result<GraphBuilder> {
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
fn ingest_object(b: &mut GraphBuilder, obj: &Value) {
    let ty = obj.get("@type").and_then(Value::as_str).unwrap_or("");
    if ty != "Product" {
        return;
    }
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
    // Stable id: prefer @id, then sku, then name (last-resort).
    let id = obj
        .get("@id")
        .or_else(|| obj.get("sku"))
        .and_then(Value::as_str)
        .unwrap_or(name)
        .to_string();
    let props = obj.to_string();
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

    if let Some(offer) = obj.get("offers") {
        let offer_obj = if offer.is_array() {
            offer.get(0)
        } else {
            Some(offer)
        };
        if let Some(o) = offer_obj {
            let offer_id = format!("{id}#offer");
            let price = o.get("price").map(|v| v.to_string()).unwrap_or_default();
            b.upsert_node(NodeKind::Offer, &offer_id, &price, &o.to_string());
            b.add_edge(product_idx, RelType::Offers, &offer_id);
        }
    }

    if let Some(rating) = obj.get("aggregateRating") {
        let rating_id = format!("{id}#rating");
        let rv = rating.get("ratingValue").map(|v| v.to_string()).unwrap_or_default();
        b.upsert_node(NodeKind::AggregateRating, &rating_id, &rv, &rating.to_string());
        b.add_edge(product_idx, RelType::AggregateRating, &rating_id);
    }
}
