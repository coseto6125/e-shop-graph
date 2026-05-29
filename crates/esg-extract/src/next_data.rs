//! Extractor for Next.js `__NEXT_DATA__`. SSR (or post-render) Next.js sites
//! embed page data in `<script id="__NEXT_DATA__" type="application/json">`.
//! Products live somewhere under `props.pageProps`, but the KEY varies per site
//! (products / items / goods / productList / ...), so we don't hardcode it: we
//! recursively find arrays of objects that look like products (have a name-ish
//! AND a price-ish field) and ingest those.

use esg_core::{GraphBuilder, NodeKind, RelType};
use serde_json::Value;

/// Parse the `__NEXT_DATA__` JSON blob, if present.
pub fn find_next_data(html: &str) -> Option<Value> {
    let needle = "id=\"__NEXT_DATA__\"";
    let at = html.find(needle)?;
    // jump to the '>' that ends the opening <script ...> tag, then to the JSON.
    let gt = html[at..].find('>')? + at + 1;
    let end = html[gt..].find("</script>")? + gt;
    serde_json::from_str(html[gt..end].trim()).ok()
}

/// Collect product-like objects from any array in the tree where every element
/// looks like a product (has both a name-ish and a price-ish key).
pub fn collect_product_arrays(root: &Value) -> Vec<Value> {
    let mut found = Vec::new();
    walk(root, &mut found);
    found
}

fn walk(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(arr) => {
            if !arr.is_empty() && arr.iter().all(is_product_like) {
                out.extend(arr.iter().cloned());
            } else {
                arr.iter().for_each(|e| walk(e, out));
            }
        }
        Value::Object(map) => map.values().for_each(|e| walk(e, out)),
        _ => {}
    }
}

fn is_product_like(v: &Value) -> bool {
    let Some(obj) = v.as_object() else {
        return false;
    };
    let has = |kinds: &[&str]| {
        obj.keys()
            .any(|k| kinds.iter().any(|n| k.to_ascii_lowercase().contains(n)))
    };
    has(&["name", "title"]) && has(&["price", "amount", "cost"])
}

/// Ingest a product-like object discovered under __NEXT_DATA__. Prices found
/// under common keys are run through the page `PriceScale` for the same
/// cents-normalization + confidence scoring as the other extractors.
pub fn ingest_next_product(b: &mut GraphBuilder, p: &Value, scale: &crate::price::PriceScale) {
    let obj = match p.as_object() {
        Some(o) => o,
        None => return,
    };
    let name = first_str(obj, &["name", "title", "productName"]).unwrap_or_default();
    // Identity priority: product id (string OR numeric) → sku → handle → name.
    // The id is the cross-view stable key shared with a listing card / detail
    // page, so it must win — and it's often a JSON number, which a string-only
    // read would miss, falling back to a slug that wouldn't dedup against the
    // numeric-id view.
    let id = first_id(obj, &["id", "productId", "sku", "handle"]).unwrap_or_else(|| name.clone());
    if id.is_empty() {
        return;
    }
    let product_idx = b.upsert_node(NodeKind::Product, &id, &name, &props_with_price(obj, scale));

    // Variant arrays under common keys.
    for vkey in ["variants", "variations", "skus", "options"] {
        if let Some(vs) = obj.get(vkey).and_then(Value::as_array) {
            for v in vs {
                let vobj = match v.as_object() {
                    Some(o) => o,
                    None => continue,
                };
                let vid = first_str(vobj, &["id", "sku"])
                    .map(|s| format!("{id}#v{s}"))
                    .unwrap_or_else(|| format!("{id}#v"));
                let vtitle = first_str(vobj, &["title", "name"]).unwrap_or_default();
                b.upsert_node(
                    NodeKind::Variant,
                    &vid,
                    &vtitle,
                    &props_with_price(vobj, scale),
                );
                b.add_edge(product_idx, RelType::HasVariant, &vid);
            }
            break;
        }
    }
}

/// Serialize an object's JSON, injecting normalized `price` / `currency` /
/// `price_confident` / `price_score` when a price field scores. Mirrors
/// `platform_json::with_normalized_price` for the __NEXT_DATA__ key shapes.
fn props_with_price(
    obj: &serde_json::Map<String, Value>,
    scale: &crate::price::PriceScale,
) -> String {
    let price_field = ["price", "salePrice", "amount", "priceValue"]
        .iter()
        .find_map(|k| obj.get(*k));
    let mut map = obj.clone();
    if let Some(v) = scale.verdict(price_field, None) {
        v.write_into(&mut map);
    }
    Value::Object(map).to_string()
}

fn first_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

/// Like `first_str` but for identity fields: accepts a JSON number as well as a
/// string (a product `id` is frequently numeric), rendered without quotes so it
/// matches the same id read as a number elsewhere. Empty strings are skipped.
fn first_id(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        obj.get(*k).and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
                .filter(|s| !s.is_empty())
        })
    })
}
