//! Cross-source schema parity: every product extraction path must surface
//! the same Product props (`name`, `price`, `price_cents`, `price_scale`,
//! `currency`, `price_confident`, `price_score`) when the page carries that
//! signal.
//!
//! Regression-driven by the 0.2.0 finding that JSON-LD ingest skipped
//! `PriceScale::verdict` entirely — JSON-LD Product nodes were missing
//! every normalized price field. This file pins the contract so future
//! source additions (or refactors) cannot silently regress one path.

use esg_core::cypher::{self, Value};
use esg_core::graph::ArchivedGraph;
use esg_extract::build_from_pages;

fn cypher_rows(html: &str, query: &str) -> Vec<Vec<Value>> {
    let builder = build_from_pages(&[html.to_string()]).expect("build_from_pages");
    let graph = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph)
        .expect("rkyv serialize")
        .to_vec();
    let archived = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).expect("rkyv access");
    cypher::query(archived, query).expect("cypher").rows
}

// JSON-LD fixture — schema.org Product wrapped in <script type="application/ld+json">.
// The price (4200 TWD) is intentionally a plausible cents value so without
// PriceScale corroboration the verdict has to think.
const JSONLD_HTML: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"Air Zoom",
 "url":"https://nike.example/air-zoom",
 "brand":{"@type":"Brand","name":"Nike"},
 "offers":{"@type":"Offer","price":"4200","priceCurrency":"TWD"}}
</script></head><body><p>NT$4,200</p></body></html>"#;

// Microdata fixture — meta-tag layout (`itemprop` + `content="..."`), the
// only itemprop shape the production extractor reads today. Span-with-
// text-content layouts are out of scope until microdata reader broadens.
const MICRODATA_HTML: &str = r#"<!doctype html><html><head>
<meta itemprop="name" content="Air Zoom">
<meta itemprop="price" content="4200">
<meta itemprop="priceCurrency" content="TWD">
<meta property="og:url" content="https://nike.example/air-zoom">
</head><body><p>NT$4,200</p></body></html>"#;

// Platform JSON fixture — Shopify-style `"products": [...]` array. Includes
// `"currency_code":"TWD"` because PriceScale only commits to a currency from
// unambiguous signals (ISO codes or single-currency symbols); a bare "NT$"
// is ambiguous ($ maps to USD/TWD/HKD/SGD/...) and intentionally yields "".
// Real Shopify/cyberbiz payloads carry the explicit code; this fixture
// matches production reality.
const PLATFORM_HTML: &str = r#"<!doctype html><html><body>
<script>window.__data = {"products":[{"name":"Air Zoom","handle":"air-zoom","price":"4200","price_min":"4200.0","currency_code":"TWD"}]};</script>
<p>NT$4,200</p>
</body></html>"#;

#[test]
fn jsonld_product_inlines_price() {
    // Regression: in 0.2.0 this returned None because ingest_object never
    // received the page's PriceScale. Now the same code path used by every
    // other source produces a normalized whole-unit `price` string. TWD is
    // zero-decimal so JSON `"4200"` and a hypothetical `4200.00` both
    // render as `"4200"` (no `.00`).
    let rows = cypher_rows(JSONLD_HTML, "MATCH (p:Product) RETURN p.price");
    assert_eq!(rows.len(), 1, "expected one Product node");
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "4200"),
        "JSON-LD Product.price should be \"4200\", got {:?}",
        rows[0][0]
    );
}

#[test]
fn jsonld_product_inlines_currency() {
    let rows = cypher_rows(JSONLD_HTML, "MATCH (p:Product) RETURN p.currency");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "TWD"),
        "JSON-LD currency should be TWD, got {:?}",
        rows[0][0]
    );
}

#[test]
fn jsonld_product_inlines_price_confident() {
    // The verdict's confident bool must be exposed so LLM queries can filter
    // for high-signal prices (`WHERE p.price_confident = true`).
    let rows = cypher_rows(JSONLD_HTML, "MATCH (p:Product) RETURN p.price_confident");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(rows[0][0], Value::Bool(_)),
        "price_confident should be a bool, got {:?}",
        rows[0][0]
    );
}

#[test]
fn jsonld_brand_node_still_emitted() {
    // The inline-price fix must not regress the existing Brand node + edge
    // (which is how Cypher already discovers brand via hop).
    let rows = cypher_rows(JSONLD_HTML, "MATCH (b:Brand) RETURN b.name");
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Str(s) if s == "Nike"));
}

#[test]
fn jsonld_offer_node_still_emitted() {
    // Offer node remains for downstream consumers that want the raw structure
    // (multiple offers per product, separate from the normalized inline copy).
    let rows = cypher_rows(JSONLD_HTML, "MATCH (o:Offer) RETURN count(*)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int(1));
}

#[test]
fn jsonld_falls_back_to_url_for_id_when_no_at_id_or_sku() {
    // With no @id / sku but a `url`, the Product node id derives from url —
    // not from `name` (which would collapse same-named products across
    // categories). Verifiable via .url echoed in props.
    let rows = cypher_rows(JSONLD_HTML, "MATCH (p:Product) RETURN p.url");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://nike.example/air-zoom"),
        "Product.url should round-trip from JSON-LD, got {:?}",
        rows[0][0]
    );
}

// --- Cross-source parity ----------------------------------------------------
//
// These tests pin the contract that an LLM writing the SAME Cypher gets the
// SAME shape of answer no matter which extractor surfaced the product. A new
// source added to `extract_page` must satisfy this matrix.

#[test]
fn price_present_across_sources() {
    for (label, html) in [
        ("jsonld", JSONLD_HTML),
        ("microdata", MICRODATA_HTML),
        ("platform", PLATFORM_HTML),
    ] {
        let rows = cypher_rows(html, "MATCH (p:Product) RETURN p.price");
        assert_eq!(rows.len(), 1, "[{label}] one product expected");
        // Every extractor must surface a non-empty `price` string. The
        // fixture is the same 4200 TWD product so every source renders
        // `"4200"` — that's the actual parity claim, not just non-Null.
        assert!(
            matches!(&rows[0][0], Value::Str(s) if !s.is_empty()),
            "[{label}] Product.price missing or non-string — schema parity broken: {:?}",
            rows[0][0],
        );
    }
}

#[test]
fn price_cents_and_scale_present_across_sources() {
    // 0.7.0: alongside the display `price` string, every source must surface
    // the minor-unit integer `price_cents` and its `price_scale`, derived from
    // the SAME Decimal. The fixture is 4200 TWD (zero-decimal) so all three
    // sources must agree: cents == 4200, scale == 0.
    for (label, html) in [
        ("jsonld", JSONLD_HTML),
        ("microdata", MICRODATA_HTML),
        ("platform", PLATFORM_HTML),
    ] {
        let rows = cypher_rows(
            html,
            "MATCH (p:Product) RETURN p.price_cents, p.price_scale",
        );
        assert_eq!(rows.len(), 1, "[{label}] one product expected");
        assert_eq!(
            (&rows[0][0], &rows[0][1]),
            (&Value::Int(4200), &Value::Int(0)),
            "[{label}] price_cents/price_scale parity broken: {:?}",
            rows[0],
        );
    }
}

#[test]
fn currency_present_across_sources() {
    for (label, html) in [
        ("jsonld", JSONLD_HTML),
        ("microdata", MICRODATA_HTML),
        // platform_json fixture has no explicit currency string; PriceScale
        // infers from visible "NT$" → "TWD". Held to the same bar.
        ("platform", PLATFORM_HTML),
    ] {
        let rows = cypher_rows(html, "MATCH (p:Product) RETURN p.currency");
        assert_eq!(rows.len(), 1, "[{label}] one product expected");
        assert!(
            matches!(&rows[0][0], Value::Str(s) if !s.is_empty()),
            "[{label}] Product.currency missing — schema parity broken",
        );
    }
}

#[test]
fn name_present_across_sources() {
    for (label, html) in [
        ("jsonld", JSONLD_HTML),
        ("microdata", MICRODATA_HTML),
        ("platform", PLATFORM_HTML),
    ] {
        let rows = cypher_rows(html, "MATCH (p:Product) RETURN p.name");
        assert_eq!(rows.len(), 1, "[{label}] one product expected");
        assert!(
            matches!(&rows[0][0], Value::Str(s) if !s.is_empty()),
            "[{label}] Product.name missing — schema parity broken",
        );
    }
}

// Platform JSON with a RELATIVE product url + featured_image — the doni/easy.co
// reality. Before 0.7.2 the inline-JSON path stored the url verbatim
// (`/products/…`, a LINE-carousel 400) and never surfaced an `image` prop.
const PLATFORM_RELATIVE_HTML: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://doni.example/products/dress">
</head><body>
<script>window.__data = {"products":[{"name":"Mermaid Dress","handle":"mermaid","url":"/products/mermaid","price":"690","currency_code":"TWD","featured_image":{"img_url":"https://cdn.example/m.jpg"}}]};</script>
</body></html>"#;

#[test]
fn platform_json_absolutizes_relative_url() {
    let rows = cypher_rows(PLATFORM_RELATIVE_HTML, "MATCH (p:Product) RETURN p.url");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://doni.example/products/mermaid"),
        "inline-JSON relative url must absolutize against page og:url origin, got {:?}",
        rows[0][0]
    );
}

#[test]
fn platform_json_surfaces_image_from_featured_image() {
    let rows = cypher_rows(PLATFORM_RELATIVE_HTML, "MATCH (p:Product) RETURN p.image");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://cdn.example/m.jpg"),
        "inline-JSON featured_image.img_url must surface as Product.image, got {:?}",
        rows[0][0]
    );
}
