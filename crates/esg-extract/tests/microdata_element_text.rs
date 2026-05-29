//! Microdata extractor must read element TEXT, not just `content="..."`.
//!
//! schema.org permits two carriers on an `itemprop` element:
//!   * Attribute-typed — `<meta itemprop="X" content="Y">`,
//!     `<img itemprop="image" src="...">`, `<a itemprop="url" href="...">`.
//!   * Element text — `<h1 itemprop="name">Y</h1>`, `<span itemprop="price">299</span>`.
//!
//! Production failure (doni / EasyStore, 2026-05-28): the storefront kept
//! `name` on `<h1>` text but `url` / `image` / `priceCurrency` on `<meta
//! content>`. The original "content-only" reader returned None for `name`
//! and `extract_from_dom` early-returned an empty vec → zero Product nodes
//! for an entire 690-product catalogue. This file pins the new contract
//! so a future refactor cannot regress it.

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

// Real-world doni layout: name on `<h1>` text, image / url / currency on
// `<meta content=...>`. Mixed-carrier microdata is the schema.org norm; an
// extractor that doesn't read element text loses this entire shop.
const DONI_HTML: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://shop.example/products/test-dress">
<meta itemprop="url" content="https://shop.example/products/test-dress">
<meta itemprop="image" content="https://cdn.example/i/test-dress.jpg">
<meta itemprop="priceCurrency" content="TWD">
</head><body>
<h1 itemprop="name">Test Dress</h1>
<span itemprop="price">890</span>
<p>NT$890</p>
</body></html>"#;

#[test]
fn name_reads_from_h1_element_text() {
    let rows = cypher_rows(DONI_HTML, "MATCH (p:Product) RETURN p.name");
    assert_eq!(rows.len(), 1, "one product expected");
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "Test Dress"),
        "Product.name should come from <h1> text, got {:?}",
        rows[0][0]
    );
}

#[test]
fn price_reads_from_span_element_text() {
    // Same lesson on price: real stores ship `<span itemprop="price">890</span>`,
    // not a `content=` meta. The display string honours the source value
    // verbatim (zero-decimal stripping for TWD) — `"890"`, never `"890.00"`
    // or a multiplied-cents integer.
    let rows = cypher_rows(DONI_HTML, "MATCH (p:Product) RETURN p.price");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "890"),
        "Product.price should be \"890\" (TWD zero-decimal), got {:?}",
        rows[0][0]
    );
}

#[test]
fn image_reads_from_meta_content_attr() {
    // The mixed-carrier scenario: image on `<meta content=...>`, name on text.
    // Existing `content=` path must keep working after the text fallback was
    // added — otherwise we trade one regression for another.
    let rows = cypher_rows(DONI_HTML, "MATCH (p:Product) RETURN p.image");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://cdn.example/i/test-dress.jpg"),
        "Product.image should come from <meta content=...>, got {:?}",
        rows[0][0]
    );
}

#[test]
fn url_reads_from_og_url_meta() {
    // Existing og:url path stays the source of truth for url. The new text
    // fallback must not steal precedence from `<meta content=...>`.
    let rows = cypher_rows(DONI_HTML, "MATCH (p:Product) RETURN p.url");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://shop.example/products/test-dress"),
        "Product.url should round-trip from og:url meta, got {:?}",
        rows[0][0]
    );
}

// A listing page renders each product as an itemscope card whose `itemprop=url`
// is a ROOT-RELATIVE href (`/products/foo`), while the page's og:url is the
// absolute form. Before the fix these forked into two nodes (relative id vs
// absolute id) and the relative one leaked a non-`https://` `uri` into a LINE
// carousel, 400-ing the whole message. The card url must absolutize against the
// page origin so it collapses onto the absolute id.
const LISTING_RELATIVE_HREF: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://shop.example/collections/dress">
</head><body>
<div itemscope itemtype="https://schema.org/Product">
  <h1 itemprop="name">Mermaid Dress</h1>
  <span itemprop="price">690</span>
  <meta itemprop="priceCurrency" content="TWD">
  <a itemprop="url" href="/products/mermaid-dress">view</a>
  <img itemprop="image" src="//cdn.example/i/mermaid.jpg">
</div>
</body></html>"#;

#[test]
fn relative_card_url_absolutized_against_page_origin() {
    let rows = cypher_rows(LISTING_RELATIVE_HREF, "MATCH (p:Product) RETURN p.url");
    assert_eq!(rows.len(), 1, "one product node, not forked relative+absolute");
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://shop.example/products/mermaid-dress"),
        "root-relative card href should be absolutized to the page origin, got {:?}",
        rows[0][0]
    );
}

#[test]
fn protocol_relative_image_borrows_page_scheme() {
    // `//cdn.example/...` (protocol-relative) must gain the page's scheme so a
    // carousel thumbnail URL is a full `https://...`, not a scheme-less string.
    let rows = cypher_rows(LISTING_RELATIVE_HREF, "MATCH (p:Product) RETURN p.image");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://cdn.example/i/mermaid.jpg"),
        "protocol-relative image should borrow page scheme, got {:?}",
        rows[0][0]
    );
}

#[test]
fn absolute_card_url_passes_through_unchanged() {
    // The dedup target: a card already carrying the absolute url must be left
    // byte-identical so the relative form collapses onto exactly this string.
    const HTML: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://shop.example/collections/dress">
</head><body>
<div itemscope itemtype="https://schema.org/Product">
  <h1 itemprop="name">Mermaid Dress</h1>
  <a itemprop="url" href="https://shop.example/products/mermaid-dress">view</a>
</div>
</body></html>"#;
    let rows = cypher_rows(HTML, "MATCH (p:Product) RETURN p.url");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://shop.example/products/mermaid-dress"),
        "absolute url must pass through unchanged, got {:?}",
        rows[0][0]
    );
}

#[test]
fn img_itemprop_uses_src_attr() {
    // `<img itemprop="image" src="...">` is the canonical schema.org carrier
    // for image — `src` is implicit on `<img>`. The reader must honour it
    // even when no `content=` is present.
    const HTML: &str = r#"<!doctype html><html><head>
<meta itemprop="name" content="X">
<meta property="og:url" content="https://shop.example/x">
</head><body>
<img itemprop="image" src="https://cdn.example/x.jpg">
</body></html>"#;
    let rows = cypher_rows(HTML, "MATCH (p:Product) RETURN p.image");
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0][0], Value::Str(s) if s == "https://cdn.example/x.jpg"),
        "Product.image should come from <img src=...>, got {:?}",
        rows[0][0]
    );
}
