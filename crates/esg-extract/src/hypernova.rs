//! Extractor for Hypernova "SUPER LANDING" one-page stores (shareco.me et al.).
//! Products live in a `<script type="application/json"
//! data-hypernova-key="landingdesktopApp"><!--{json}--></script>` block — the
//! JSON is wrapped in an HTML COMMENT, so the cheap `"products":[` substring
//! scan that platform_json relies on never reaches it. The shape is
//! `props.page.products[]` (each: id/title/price/originalPrice/image +
//! imageDesc1..5 gallery), and `props.page.url` is the canonical page URL — a
//! one-page store, so every product links to that single landing page.

use crate::{has_product_signal, normalize, price::PriceScale};
use esg_core::{GraphBuilder, NodeKind};
use serde_json::{json, Value};

const HYPERNOVA_KEY: &str = "data-hypernova-key=\"landingdesktopApp\"";

/// Locate the Hypernova `<script>` data block, strip its HTML-comment wrapper,
/// parse it, and return `(props.page.products, props.page.url)`. None when the
/// page has no Hypernova block or the JSON doesn't carry a products array.
///
/// The key appears twice on the page — once on the `<div>` mount point, once on
/// the `<script>` data block — so we accept only the occurrence whose enclosing
/// tag is a `<script>`.
pub fn find_hypernova(html: &str) -> Option<(Vec<Value>, String)> {
    let mut search_from = 0;
    while let Some(rel) = html[search_from..].find(HYPERNOVA_KEY) {
        let key_at = search_from + rel;
        let tag_start = html[..key_at].rfind('<').unwrap_or(0);
        if !html[tag_start..].starts_with("<script") {
            search_from = key_at + HYPERNOVA_KEY.len();
            continue;
        }
        let gt = html[key_at..].find('>')? + key_at + 1;
        let end = html[gt..].find("</script>")? + gt;
        // The body is HTML-comment-wrapped: `<!--{…}-->`. Strip both markers if
        // present (trim whatever is there if only one side is).
        let inner = html[gt..end]
            .trim()
            .trim_start_matches("<!--")
            .trim_end_matches("-->")
            .trim();
        let root: Value = serde_json::from_str(inner).ok()?;
        let page = root.get("props")?.get("page")?;
        let products = page.get("products")?.as_array()?.clone();
        // `props.page.url` is the per-page canonical (the one-page storefront).
        let canonical = page
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Some((products, canonical));
    }
    None
}

/// Ingest Hypernova products as Product nodes. Each carries its own price/image;
/// the gallery (image + imageDesc1..5) is folded through the SAME normalize path
/// as the other extractors. Every product's `url` is the page canonical (the
/// one-page store IS the listing page — the carousel link opens it).
pub fn ingest_hypernova(
    b: &mut GraphBuilder,
    products: &[Value],
    scale: &PriceScale,
    canonical: &str,
    origin: Option<&str>,
) {
    for p in products {
        let Some(obj) = p.as_object() else {
            continue;
        };
        let name = obj.get("title").and_then(Value::as_str).unwrap_or("");
        // id is an int in the sample; render without quotes so it dedups against
        // any numeric-id view, falling back to name when absent.
        let id = obj
            .get("id")
            .and_then(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.to_string());
        if id.is_empty() {
            continue;
        }

        let mut pm = serde_json::Map::new();
        if let Some(verdict) = scale.verdict(obj.get("price"), None) {
            verdict.write_into(&mut pm);
        }

        // Build the gallery: primary `image` + imageDesc1..5, reusing the shared
        // normalize logic (dedup + junk-filter) via a temp view object.
        let gallery: Vec<Value> = [
            "imageDesc1",
            "imageDesc2",
            "imageDesc3",
            "imageDesc4",
            "imageDesc5",
        ]
        .iter()
        .filter_map(|k| obj.get(*k))
        .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        .cloned()
        .collect();
        let view = json!({ "image": obj.get("image"), "images": gallery });
        if let Some(img) = normalize::extract_image(&view) {
            pm.insert(
                "image".into(),
                normalize::absolutize_url(&img, origin).into(),
            );
        }
        let images = normalize::extract_images(&view);
        if images.len() > 1 {
            pm.insert(
                "images".into(),
                Value::Array(
                    images
                        .into_iter()
                        .map(|u| normalize::absolutize_url(&u, origin).into())
                        .collect(),
                ),
            );
        }

        // One-page store: every product's link target is the page canonical.
        if !canonical.is_empty() {
            pm.insert("url".into(), canonical.into());
        }

        if !has_product_signal(&pm) {
            continue;
        }
        let props = Value::Object(pm).to_string();
        b.upsert_node(NodeKind::Product, &id, name, &props);
    }
}
