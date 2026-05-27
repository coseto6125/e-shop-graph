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

/// Ingest a product-like object discovered under __NEXT_DATA__.
pub fn ingest_next_product(b: &mut GraphBuilder, p: &Value) {
    let obj = match p.as_object() {
        Some(o) => o,
        None => return,
    };
    let name = first_str(obj, &["name", "title", "productName"]).unwrap_or_default();
    let id = first_str(obj, &["id", "sku", "handle", "productId"]).unwrap_or_else(|| name.clone());
    if id.is_empty() {
        return;
    }
    let product_idx = b.upsert_node(NodeKind::Product, &id, &name, &p.to_string());

    // Variant arrays under common keys.
    for vkey in ["variants", "variations", "skus", "options"] {
        if let Some(vs) = obj.get(vkey).and_then(Value::as_array) {
            for v in vs {
                let vid = v
                    .as_object()
                    .and_then(|o| first_str(o, &["id", "sku"]))
                    .map(|s| format!("{id}#v{s}"))
                    .unwrap_or_else(|| format!("{id}#v"));
                let vtitle = v
                    .as_object()
                    .and_then(|o| first_str(o, &["title", "name"]))
                    .unwrap_or_default();
                b.upsert_node(NodeKind::Variant, &vid, &vtitle, &v.to_string());
                b.add_edge(product_idx, RelType::HasVariant, &vid);
            }
            break;
        }
    }
}

fn first_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}
