//! Extractor for platform-embedded product JSON — the `"products":[...]` array
//! that Shopify-like stores (incl. easy.co) inline in the page. This is the
//! richest, cleanest source in the wild: real sites (e.g. doni easy.co) ship
//! ZERO schema.org JSON-LD but a full products array with variants.
//!
//! Schema observed on doni easy.co (product object):
//!   id, handle, name, title, url, price, price_min/max, compare_at_price,
//!   available, options_with_values, variants[], featured_image, metafields
//! variant: id, sku, price, compare_at_price, available, inventory_quantity,
//!   option1/2/3, title

use crate::price::{self, PriceLevel};
use esg_core::{GraphBuilder, NodeKind, RelType};
use serde_json::Value;

/// Find and parse the first balanced `"products":[ ... ]` array in the page.
/// Returns the parsed array, or None if absent / unparseable.
pub fn find_products_array(html: &str) -> Option<Vec<Value>> {
    let key = html.find("\"products\"")?;
    // Advance to the opening '[' after the key (skip `":` and whitespace).
    let bracket = html[key..].find('[')? + key;
    let bytes = html.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut end = None;
    for (off, &b) in bytes[bracket..].iter().enumerate() {
        let c = b as char;
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(bracket + off + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let arr_text = &html[bracket..end?];
    serde_json::from_str::<Vec<Value>>(arr_text).ok()
}

/// Ingest one platform product object into the graph: a Product node plus one
/// Variant node per `variants[]` entry, linked by HasVariant.
pub fn ingest_product(b: &mut GraphBuilder, p: &Value) {
    let name = p.get("name").or_else(|| p.get("title")).and_then(Value::as_str).unwrap_or("");
    // Stable id: handle is the platform's slug; fall back to numeric id.
    let id = p
        .get("handle")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| p.get("id").map(|v| v.to_string()))
        .unwrap_or_else(|| name.to_string());
    if id.is_empty() {
        return;
    }
    // Currency resolved once per product; variants inherit it.
    let currency = price::resolve_currency(p, "TWD");
    let product_props = with_normalized_price(p, p.get("price"), PriceLevel::Product, &currency);
    let product_idx = b.upsert_node(NodeKind::Product, &id, name, &product_props);

    if let Some(variants) = p.get("variants").and_then(Value::as_array) {
        for v in variants {
            let vid = v
                .get("id")
                .map(|x| format!("{id}#v{x}"))
                .unwrap_or_else(|| format!("{id}#v"));
            let vtitle = v.get("title").and_then(Value::as_str).unwrap_or("");
            let vprops = with_normalized_price(v, v.get("price"), PriceLevel::Variant, &currency);
            b.upsert_node(NodeKind::Variant, &vid, vtitle, &vprops);
            b.add_edge(product_idx, RelType::HasVariant, &vid);
        }
    }
}

/// Serialize a node's source object with normalized `price_cents` + `currency`
/// injected, so queries see unit-consistent prices. Leaves the original `price`
/// field intact for provenance.
fn with_normalized_price(
    obj: &Value,
    price_field: Option<&Value>,
    level: PriceLevel,
    currency: &str,
) -> String {
    let mut map = obj.as_object().cloned().unwrap_or_default();
    if let Some(cents) = price::to_cents(price_field, level) {
        map.insert("price_cents".into(), Value::Number(cents.into()));
    }
    map.insert("currency".into(), Value::String(currency.to_string()));
    Value::Object(map).to_string()
}
