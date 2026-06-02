//! Extractor for platform-embedded product JSON — the `"products":[...]` array
//! that Shopify-like stores (incl. easy.co) inline in the page. This is the
//! richest, cleanest source in the wild: real sites (e.g. doni easy.co) ship
//! ZERO schema.org JSON-LD but a full products array with variants.
//!
//! Variant models differ by platform (verified on real stores) — there is NO
//! single field name, so we detect the shape rather than hardcode one:
//!   A. `variants[]`      — doni/easy.co: array of {id,sku,price,option1..3,title}
//!   B. `variations`      — shopline: array (often surfaced in DOM, SPA-rendered)
//!   C. `price_range{min,max}` — cyberbiz: no variant array, just a price band
//! All three normalize onto the same Product→Variant graph shape.

use crate::normalize;
use crate::price::PriceScale;
use esg_core::{GraphBuilder, NodeKind, RelType};
use rust_decimal::Decimal;
use serde_json::Value;

/// Candidate keys for the inline product array, in priority order. `products`
/// is the dominant Shopify-compatible name; the rest cover stores that relabel
/// it. Tried in order — the first that parses as a NON-EMPTY array wins, so a
/// broader name like `goods` only acts as a fallback and never shadows
/// `products` on a page that has both.
///
/// `"items"` was deliberately dropped: it is too generic and false-positively
/// matched non-product arrays on real stores — cyberbiz inlines a
/// `product_labels.items` config array (`{"kind":"system","title":"特價標籤"}`)
/// and shopline a `filter_tag` `items` array (`{"content_translations":…,"count":8}`),
/// either of which preempted the real products. No verified store uses `items`
/// as the genuine product array.
const PRODUCT_ARRAY_KEYS: [&str; 3] = ["\"products\"", "\"productList\"", "\"goods\""];

/// Find and parse the first balanced product array in the page, trying each
/// candidate key in priority order. Returns the first array that both parses
/// and looks like products, or None.
pub fn find_products_array(html: &str) -> Option<Vec<Value>> {
    PRODUCT_ARRAY_KEYS.iter().find_map(|key| {
        parse_array_after_key(html, key).filter(|a| !a.is_empty() && looks_like_product_array(a))
    })
}

/// Reject an array that parses but is plainly NOT products — a config / label
/// array that happened to sit under a matching key. The discriminator is the
/// `kind` field cyberbiz stamps on label definitions (`"kind":"system"` /
/// `"custom"`), never present on a real product. Belt-and-suspenders alongside
/// dropping the `items` key: defends `products`/`goods` against the same class
/// of false positive without needing to know every relabelled key.
fn looks_like_product_array(arr: &[Value]) -> bool {
    !arr.iter()
        .all(|v| v.get("kind").and_then(Value::as_str).is_some())
}

/// Parse the first balanced `[ ... ]` array following `key` in `html`.
fn parse_array_after_key(html: &str, key: &str) -> Option<Vec<Value>> {
    let pos = html.find(key)?;
    // Advance to the opening '[' after the key (skip `":` and whitespace).
    let bracket = html[pos..].find('[')? + pos;
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
/// Variant node per `variants[]` entry, linked by HasVariant. `scale` carries
/// the page's visible prices, used to infer each JSON number's unit/currency.
pub fn ingest_product(b: &mut GraphBuilder, p: &Value, scale: &PriceScale, origin: Option<&str>) {
    let name = p
        .get("name")
        .or_else(|| p.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("");
    // Identity priority: numeric product id → handle → name. The product id is
    // the cross-view stable key — a detail page (microdata) and this listing
    // card carry the SAME id, while their urls/handles can differ
    // (collection-scoped vs bare), so keying on id collapses what would
    // otherwise be two nodes for one product. handle (the platform slug) is the
    // fallback for arrays that omit a numeric id; name is last resort.
    let id = p
        .get("id")
        .and_then(|v| {
            v.as_i64()
                .map(|n| n.to_string())
                .or_else(|| v.as_str().map(str::to_string))
        })
        .filter(|s| !s.is_empty())
        .or_else(|| p.get("handle").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| name.to_string());
    if id.is_empty() {
        return;
    }
    // Whole-unit peer for symbol-independent price corroboration. Prefer a
    // decimal string `price_min` ("790.0" — the dot proves whole units), else
    // a numeric price_range.min.price.
    let peer_whole = whole_unit_peer(p);
    let mut props = normalized_price_map(p, p.get("price"), scale, peer_whole);
    // Absolutize the inline-JSON `url` (storefronts emit a root-relative
    // `/products/…`, which 400s a LINE carousel uri) and surface a single
    // `image` prop from whatever shape the source used (featured_image / images[]).
    // Same normalization the microdata path applies — kept here so the JSON
    // path doesn't drift back to relative URLs + no thumbnail.
    if let Some(u) = p.get("url").and_then(Value::as_str) {
        props.insert("url".into(), normalize::absolutize_url(u, origin).into());
    }
    if let Some(img) = normalize::extract_image(p) {
        props.insert(
            "image".into(),
            normalize::absolutize_url(&img, origin).into(),
        );
    }
    let product_props = Value::Object(props).to_string();
    let product_idx = b.upsert_node(NodeKind::Product, &id, name, &product_props);

    ingest_variants(b, p, &id, product_idx, scale, peer_whole);
}

/// Emit Variant nodes from whichever variant model the product uses.
fn ingest_variants(
    b: &mut GraphBuilder,
    p: &Value,
    id: &str,
    product_idx: u32,
    scale: &PriceScale,
    peer_whole: Option<Decimal>,
) {
    // Model A/B: an array under `variants` or `variations`.
    let array = p
        .get("variants")
        .or_else(|| p.get("variations"))
        .and_then(Value::as_array);
    if let Some(variants) = array.filter(|a| !a.is_empty()) {
        for v in variants {
            let vid = v
                .get("id")
                .map(|x| format!("{id}#v{x}"))
                .unwrap_or_else(|| format!("{id}#v"));
            let vtitle = v.get("title").and_then(Value::as_str).unwrap_or("");
            let vprops = with_normalized_price(v, v.get("price"), scale, peer_whole);
            b.upsert_node(NodeKind::Variant, &vid, vtitle, &vprops);
            b.add_edge(product_idx, RelType::HasVariant, &vid);
        }
        return;
    }
    // Model C: no variant array, only a `price_range{min,max}`. Emit a single
    // Variant carrying the band so price queries still have a node to hit.
    if let Some(range) = p.get("price_range") {
        let vid = format!("{id}#range");
        let band_price = range.get("min").and_then(|m| m.get("price"));
        let vprops = with_normalized_price(range, band_price, scale, peer_whole);
        b.upsert_node(NodeKind::Variant, &vid, "price_range", &vprops);
        b.add_edge(product_idx, RelType::HasVariant, &vid);
    }
}

/// A value KNOWN to be in whole units, for cross-field price corroboration.
/// A decimal-string `price_min` is the strongest (the dot proves whole units);
/// `price_range.min.price` is the cyberbiz fallback. Returned as `Decimal`
/// so the downstream comparison stays exact — `f64` here once turned 690.00
/// into 689.9999… and the corroboration silently mis-fired.
fn whole_unit_peer(p: &Value) -> Option<Decimal> {
    if let Some(s) = p.get("price_min").and_then(Value::as_str) {
        if let Ok(v) = Decimal::from_str_exact(s) {
            return Some(v);
        }
    }
    p.get("price_range")
        .and_then(|r| r.get("min"))
        .and_then(|m| m.get("price"))
        .and_then(|v| match v {
            Value::Number(n) => Decimal::from_str_exact(&n.to_string()).ok(),
            Value::String(s) => Decimal::from_str_exact(s).ok(),
            _ => None,
        })
}

/// Serialize a node's source object with normalized `price`, `currency`,
/// `price_confident`, and `price_score` injected. Unit is scored from
/// independent signals (JSON cross-field via `peer_whole`, page anchors,
/// recurrence) per `PriceVerdict`. The original JSON `price` key is
/// overwritten with the rendered whole-unit string — what the source meant
/// to display — so a downstream `RETURN p.price` works without per-currency
/// math.
///
/// Crate-visible so JSON-LD ingest (`lib::ingest_object`) shares the same
/// normalization path as every other source — one price contract across
/// platform_json / dom_attr / next_data / microdata / jsonld.
pub(crate) fn with_normalized_price(
    obj: &Value,
    price_field: Option<&Value>,
    scale: &PriceScale,
    peer_whole: Option<Decimal>,
) -> String {
    Value::Object(normalized_price_map(obj, price_field, scale, peer_whole)).to_string()
}

/// As `with_normalized_price` but returns the `Map` so a caller that needs to
/// inject further fields (JSON-LD ingest merges offer/rating/identity scalars)
/// can do so without a serialize→parse round-trip on the way through.
pub(crate) fn normalized_price_map(
    obj: &Value,
    price_field: Option<&Value>,
    scale: &PriceScale,
    peer_whole: Option<Decimal>,
) -> serde_json::Map<String, Value> {
    let mut map = obj.as_object().cloned().unwrap_or_default();
    if let Some(v) = scale.verdict(price_field, peer_whole) {
        v.write_into(&mut map);
    }
    map
}
