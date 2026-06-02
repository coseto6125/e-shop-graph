//! Regression coverage for the price/image/url extraction fixes (v0.x), driven
//! by real small/mid Taiwanese storefronts (cyberbiz / shopline) whose products
//! reached the graph name-only before:
//!
//!   1. `"items"` dropped from PRODUCT_ARRAY_KEYS — cyberbiz `product_labels`
//!      and shopline `filter_tag` arrays no longer masquerade as products.
//!   2. JSON-LD path surfaces `image` and accepts `AggregateOffer.lowPrice`.
//!   3. `extract_image` reads a singular `image:[url,…]` array.
//!   4. A `ga-product` (shopline) page also folds in its JSON-LD subject — a
//!      detail page's price/image live ONLY in JSON-LD; ga carries recommends.
//!   5. `<link rel=canonical>` becomes `Product.url` when JSON-LD omits a url.
//!
//! Fixtures are minimal hand-built pages in the shape the real stores emit
//! (the real captured HTML is multi-hundred-KB and not committed).

use esg_core::cypher::{self, Value};
use esg_core::graph::ArchivedGraph;
use esg_extract::build_from_pages;

fn rows(html: &str, query: &str) -> Vec<Vec<Value>> {
    let builder = build_from_pages(&[html.to_string()]).expect("build_from_pages");
    let graph = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph)
        .expect("rkyv serialize")
        .to_vec();
    let archived = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).expect("rkyv access");
    cypher::query(archived, query).expect("cypher").rows
}

fn one_str(html: &str, query: &str) -> Option<String> {
    rows(html, query).into_iter().next().and_then(|r| {
        r.into_iter().next().and_then(|v| match v {
            Value::Str(s) => Some(s),
            _ => None,
        })
    })
}

fn count(html: &str, query: &str) -> usize {
    rows(html, query).len()
}

// ── Spec 1: cyberbiz label-config array under "items" must NOT become products,
// and the real product's JSON-LD detail must be reached instead. ──────────────
const CYBERBIZ_DETAIL: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="http://demo.cyberbiz.co/products/oreo">
<link rel="canonical" href="http://demo.cyberbiz.co/products/oreo">
<script>window.theme = {"product_labels":{"items":[
 {"id":"sale_label","kind":"system","title":"特價標籤"},
 {"id":"sold_out","kind":"system","title":"缺貨標籤"},
 {"id":"custom_1","kind":"custom","title":"自定義標籤 1"}]}};</script>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"OREO牛雪小禮盒",
 "image":"//cdn-general.cybassets.com/media/x.jpeg",
 "offers":{"@type":"Offer","price":"380","priceCurrency":"TWD"}}
</script></head><body><p>NT$380</p></body></html>"#;

#[test]
fn cyberbiz_labels_are_not_products() {
    // No label leaks in as a Product.
    assert_eq!(
        count(
            CYBERBIZ_DETAIL,
            "MATCH (p:Product) WHERE p.name = '特價標籤' RETURN p.name"
        ),
        0
    );
    // The real product is the only Product, with price + image.
    assert_eq!(count(CYBERBIZ_DETAIL, "MATCH (p:Product) RETURN p.name"), 1);
    assert_eq!(
        one_str(CYBERBIZ_DETAIL, "MATCH (p:Product) RETURN p.price").as_deref(),
        Some("380")
    );
    assert!(one_str(CYBERBIZ_DETAIL, "MATCH (p:Product) RETURN p.image").is_some());
}

// ── Spec 2: AggregateOffer.lowPrice is used when Offer.price is absent. ────────
const AGGREGATE_OFFER: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"棉質休閒洋裝",
 "image":"https://img.meepshop.me/x.jpg","url":"https://store.meepshop.me/p/x",
 "offers":{"@type":"AggregateOffer","lowPrice":790,"highPrice":1290,"priceCurrency":"TWD"}}
</script></head><body><p>NT$790</p></body></html>"#;

#[test]
fn aggregate_offer_low_price_becomes_price() {
    assert_eq!(
        one_str(AGGREGATE_OFFER, "MATCH (p:Product) RETURN p.price").as_deref(),
        Some("790")
    );
    assert_eq!(
        one_str(AGGREGATE_OFFER, "MATCH (p:Product) RETURN p.currency").as_deref(),
        Some("TWD")
    );
    assert_eq!(
        one_str(AGGREGATE_OFFER, "MATCH (p:Product) RETURN p.image").as_deref(),
        Some("https://img.meepshop.me/x.jpg")
    );
}

// ── Spec 3: JSON-LD `image` as a singular array of URL strings resolves. ──────
const IMAGE_ARRAY: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"Air Zoom",
 "url":"https://nike.example/air-zoom",
 "image":["https://cdn/a.jpg","https://cdn/b.png"],
 "offers":{"@type":"Offer","price":"4200","priceCurrency":"TWD"}}
</script></head><body><p>NT$4,200</p></body></html>"#;

#[test]
fn jsonld_singular_image_array_resolves_first() {
    assert_eq!(
        one_str(IMAGE_ARRAY, "MATCH (p:Product) RETURN p.image").as_deref(),
        Some("https://cdn/a.jpg")
    );
}

// ── Spec 4 + 5: a shopline DETAIL page — ga-product recommendation widgets +
// the JSON-LD page subject. The subject's price/image come from JSON-LD; its
// url from <link rel=canonical> (JSON-LD has none). ───────────────────────────
const SHOPLINE_DETAIL: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://www.chatzutang.com/products/rosskastanie">
<link rel="canonical" href="https://www.chatzutang.com/products/rosskastanie">
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"馬栗樹賦活護髮素",
 "image":["https://img.shoplineapp.com/media/x/original.jpg?1773971480"],
 "offers":{"@type":"Offer","price":880.0,"priceCurrency":"TWD"}}
</script></head><body>
<div ga-product='{"id":"rec1","sku":"R1","variations":"[]","title":"推薦商品一"}'></div>
<div ga-product='{"id":"rec2","sku":"R2","variations":"[]","title":"推薦商品二"}'></div>
</body></html>"#;

#[test]
fn shopline_detail_subject_captured_via_jsonld_augment() {
    // ga recommendation products + the JSON-LD subject all present.
    assert!(count(SHOPLINE_DETAIL, "MATCH (p:Product) RETURN p.name") >= 3);
    // The subject (JSON-LD only) carries price + image + canonical url.
    let q = "MATCH (p:Product) WHERE p.name = '馬栗樹賦活護髮素' RETURN ";
    assert_eq!(
        one_str(SHOPLINE_DETAIL, &format!("{q}p.price")).as_deref(),
        Some("880")
    );
    assert_eq!(
        one_str(SHOPLINE_DETAIL, &format!("{q}p.image")).as_deref(),
        Some("https://img.shoplineapp.com/media/x/original.jpg?1773971480")
    );
    assert_eq!(
        one_str(SHOPLINE_DETAIL, &format!("{q}p.url")).as_deref(),
        Some("https://www.chatzutang.com/products/rosskastanie")
    );
}

#[test]
fn canonical_not_stamped_when_jsonld_has_url() {
    // The image-array fixture's JSON-LD already has its own url — canonical
    // fallback must not overwrite it (here there's no <link canonical> anyway,
    // but the product url must be its own declared one).
    assert_eq!(
        one_str(IMAGE_ARRAY, "MATCH (p:Product) RETURN p.url").as_deref(),
        Some("https://nike.example/air-zoom")
    );
}

// ── Multi-signal gate: a @type:Product block with NO price / image / sku /
// /products/ url is an info/FAQ page mislabelled as a product (real-world:
// easy.co /blogs/news/常見問題). It must NOT become a Product node. ───────────
const FAQ_AS_PRODUCT: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"DONI",
 "url":"https://shop.easy.co/blogs/news/常見問題"}
</script></head><body><h1>常見問題</h1></body></html>"#;

#[test]
fn faq_page_mislabelled_product_is_skipped() {
    assert_eq!(count(FAQ_AS_PRODUCT, "MATCH (p:Product) RETURN p.name"), 0);
}

// A genuine product whose price is JS-rendered (absent from static HTML) still
// has an image + a /products/ url — the gate must KEEP it.
const PRICELESS_REAL_PRODUCT: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"純棉上衣",
 "url":"https://shop.easy.co/products/cotton-tee",
 "image":"https://cdn/tee.jpg"}
</script></head><body></body></html>"#;

#[test]
fn priceless_product_with_image_and_product_url_is_kept() {
    assert_eq!(
        count(PRICELESS_REAL_PRODUCT, "MATCH (p:Product) RETURN p.name"),
        1
    );
    assert_eq!(
        one_str(PRICELESS_REAL_PRODUCT, "MATCH (p:Product) RETURN p.image").as_deref(),
        Some("https://cdn/tee.jpg")
    );
}

// ── Multi-image: a product gallery (images[] of N>1) is captured as `images`,
// with `image` remaining the primary (first). doni/easy.co ships `images[]` as
// objects {img_url,…}; JSON-LD ships a singular `image` URL-string array. ─────
const MULTI_IMAGE_PLATFORM: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://shop.easy.co/products/tee"></head><body>
<script>var data = {"products":[{"id":42,"title":"純棉上衣","price":"490",
 "url":"/products/tee",
 "featured_image":{"img_url":"https://cdn/hero.jpg"},
 "images":[{"img_url":"https://cdn/hero.jpg"},{"img_url":"https://cdn/back.jpg"},{"img_url":"https://cdn/detail.jpg"}]}]};</script>
</body></html>"#;

#[test]
fn multi_image_gallery_captured() {
    // primary stays the hero
    assert_eq!(
        one_str(MULTI_IMAGE_PLATFORM, "MATCH (p:Product) RETURN p.image").as_deref(),
        Some("https://cdn/hero.jpg")
    );
    // images holds the deduped gallery (hero appears once, not twice)
    let r = rows(MULTI_IMAGE_PLATFORM, "MATCH (p:Product) RETURN p.images");
    let imgs = r.into_iter().next().and_then(|row| row.into_iter().next());
    match imgs {
        Some(Value::List(a)) => {
            let urls: Vec<&str> = a
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                urls,
                vec![
                    "https://cdn/hero.jpg",
                    "https://cdn/back.jpg",
                    "https://cdn/detail.jpg"
                ]
            );
        }
        other => panic!("expected images list, got {other:?}"),
    }
}
