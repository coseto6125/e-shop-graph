//! Extractor for product data embedded in DOM ATTRIBUTES rather than a JSON
//! array. SSR storefronts (verified on shopline) render a product card per item
//! with a `ga-product='{"id":...,"sku":...,"variations":...,"title":...}'`
//! attribute — the basic product record is in the HTML even though the page is
//! otherwise an SPA. This is the "Layer 2" source between inline `products[]`
//! JSON (Layer 1) and pure API-async pages (Layer 3, needs rendering).

use crate::price::PriceScale;
use esg_core::{GraphBuilder, NodeKind, RelType};
use serde_json::Value;

/// Collect every `ga-product='{...}'` JSON object in the page. Returns parsed
/// objects; empty when the attribute is absent.
pub fn find_ga_products(html: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let needle = "ga-product='";
    let mut rest = html;
    while let Some(start) = rest.find(needle) {
        let after = &rest[start + needle.len()..];
        if let Some(end) = after.find('\'') {
            if let Ok(v) = serde_json::from_str::<Value>(&after[..end]) {
                out.push(v);
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// Ingest a DOM-attribute product object. These carry id/sku/title/variations
/// but typically NO price (price is a separate DOM node / JS-filled), so we
/// don't fabricate one — the node still answers identity/relation queries, and
/// price can be backfilled once a renderer supplies it.
pub fn ingest_ga_product(b: &mut GraphBuilder, p: &Value, scale: &PriceScale) {
    let id = p.get("id").and_then(Value::as_str).unwrap_or("");
    let title = p.get("title").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        return;
    }
    let product_idx = b.upsert_node(NodeKind::Product, id, title, &p.to_string());

    // `variations` here is a STRING containing a JSON array (shopline quirk):
    //   "variations":"[{\"key\":...,\"sku\":...}]"
    // Parse it; each entry becomes a Variant.
    let variations = p
        .get("variations")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
        .unwrap_or_default();
    for v in &variations {
        let key = v.get("key").and_then(Value::as_str).unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let vid = format!("{id}#v{key}");
        let vprops = with_price(v, scale);
        b.upsert_node(NodeKind::Variant, &vid, "", &vprops);
        b.add_edge(product_idx, RelType::HasVariant, &vid);
    }
}

fn with_price(obj: &Value, scale: &PriceScale) -> String {
    let mut map = obj.as_object().cloned().unwrap_or_default();
    if let Some(verdict) = scale.verdict(obj.get("price"), None) {
        verdict.write_into(&mut map);
    }
    Value::Object(map).to_string()
}
