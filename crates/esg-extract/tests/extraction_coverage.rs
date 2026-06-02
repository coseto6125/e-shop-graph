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
fn shopline_detail_subject_captured_via_jsonld_first() {
    // SHOPLINE detail pages carry a clean JSON-LD Product (the page subject),
    // so the JSON-LD-first arm now wins and short-circuits BEFORE dom_attr —
    // the page's own subject is captured directly from its structured data,
    // with full price/image/url parity. The `ga-product` divs on a DETAIL page
    // are recommendation-widget products (no price/image of their own); they're
    // legitimately captured on their OWN detail/listing pages, so the subject
    // being the single Product here is correct, not a regression.
    assert_eq!(count(SHOPLINE_DETAIL, "MATCH (p:Product) RETURN p.name"), 1);
    // The subject (JSON-LD) carries price + image + canonical url.
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

// ── JSON-LD-first reorder (meepShop): a detail page has BOTH a clean JSON-LD
// Product AND a __NEXT_DATA__ productList. Pre-fix, next_data (layer 3) fired
// first and minted an EMPTY node (name="", price=null). JSON-LD-first now wins,
// so the product carries its real name + price. ──────────────────────────────
const MEEPSHOP_NEXTDATA_AND_LD: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://store.meepshop.me/p/dress">
<link rel="canonical" href="https://store.meepshop.me/p/dress">
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"棉質休閒洋裝",
 "image":"https://img.meepshop.me/d.jpg",
 "offers":{"@type":"Offer","price":"359","priceCurrency":"TWD"}}
</script>
<script id="__NEXT_DATA__" type="application/json">
{"props":{"pageProps":{"productList":[{"id":"x1","name":"","price":null}]}}}
</script></head><body><p>NT$359</p></body></html>"#;

#[test]
fn test_meepshop_jsonld_first_wins_over_nextdata() {
    // Exactly one Product — the next_data empty node never gets a chance.
    assert_eq!(count(MEEPSHOP_NEXTDATA_AND_LD, "MATCH (p:Product) RETURN p.name"), 1);
    assert_eq!(
        one_str(MEEPSHOP_NEXTDATA_AND_LD, "MATCH (p:Product) RETURN p.name").as_deref(),
        Some("棉質休閒洋裝")
    );
    assert_eq!(
        one_str(MEEPSHOP_NEXTDATA_AND_LD, "MATCH (p:Product) RETURN p.price").as_deref(),
        Some("359")
    );
}

// ── JSON-LD-first cleanliness gate (Cyberbiz/WACA): a page whose FIRST JSON-LD
// object is a BreadcrumbList (a "首頁"/Home crumb) plus microdata that would
// mis-fire on the crumb. is_clean_product_ld rejects the BreadcrumbList, so the
// real clean Product wins — the product name is NOT the crumb. ────────────────
const BREADCRUMB_PLUS_PRODUCT_LD: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"BreadcrumbList","itemListElement":[
 {"@type":"ListItem","position":1,"name":"首頁","item":"https://shop/"}]}
</script>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"真正的洋裝",
 "image":"https://cdn/dress.jpg",
 "offers":{"@type":"Offer","price":"600","priceCurrency":"TWD"}}
</script></head>
<body itemscope itemtype="https://schema.org/BreadcrumbList">
 <span itemprop="name">首頁</span>
</body></html>"#;

#[test]
fn test_cyberbiz_breadcrumb_only_ld_does_not_win_falls_to_real_product() {
    // The crumb never becomes a Product.
    assert_eq!(
        count(
            BREADCRUMB_PLUS_PRODUCT_LD,
            "MATCH (p:Product) WHERE p.name = '首頁' RETURN p.name"
        ),
        0
    );
    // The real product is captured with its price.
    assert_eq!(
        one_str(BREADCRUMB_PLUS_PRODUCT_LD, "MATCH (p:Product) RETURN p.name").as_deref(),
        Some("真正的洋裝")
    );
    assert_eq!(
        one_str(BREADCRUMB_PLUS_PRODUCT_LD, "MATCH (p:Product) RETURN p.price").as_deref(),
        Some("600")
    );
}

// ── ProductGroup.hasVariant descent (Shopify): top-level @type is ProductGroup
// (no price/image of its own) with real products nested under hasVariant[].
// Pre-fix esg extracted 0 nodes. Now: one Product per variant, each with its
// own sku + price + image (incl. an OutOfStock variant). ──────────────────────
const SHOPIFY_PRODUCT_GROUP: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://jiwudoc.myshopify.com/products/dress">
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"ProductGroup","productGroupID":"QZC007","name":"露肩洋裝",
 "hasVariant":[
  {"@type":"Product","name":"露肩洋裝 - 紫紅","sku":"QZC007-PNK",
   "image":"https://cdn/pnk.jpg",
   "offers":{"@type":"Offer","price":"680.00","priceCurrency":"TWD","availability":"http://schema.org/OutOfStock"}},
  {"@type":"Product","name":"露肩洋裝 - 白","sku":"QZC007-WHT",
   "image":"https://cdn/wht.jpg",
   "offers":{"@type":"Offer","price":"720.00","priceCurrency":"TWD","availability":"http://schema.org/InStock"}},
  {"@type":"Product","name":"露肩洋裝 - 黑","sku":"QZC007-BLK",
   "image":"https://cdn/blk.jpg",
   "offers":{"@type":"Offer","price":"720.00","priceCurrency":"TWD","availability":"http://schema.org/InStock"}}
 ]}
</script></head><body></body></html>"#;

#[test]
fn test_shopify_productgroup_hasvariant_emits_per_variant_products() {
    // 3 variants -> 3 Products.
    assert_eq!(count(SHOPIFY_PRODUCT_GROUP, "MATCH (p:Product) RETURN p.name"), 3);
    // Distinct skus preserved.
    assert_eq!(
        count(
            SHOPIFY_PRODUCT_GROUP,
            "MATCH (p:Product) WHERE p.sku = 'QZC007-PNK' RETURN p.sku"
        ),
        1
    );
    // The OutOfStock variant keeps its own price + availability.
    let q = "MATCH (p:Product) WHERE p.sku = 'QZC007-PNK' RETURN ";
    assert_eq!(
        one_str(SHOPIFY_PRODUCT_GROUP, &format!("{q}p.price")).as_deref(),
        Some("680")
    );
    assert_eq!(
        one_str(SHOPIFY_PRODUCT_GROUP, &format!("{q}p.availability")).as_deref(),
        Some("OutOfStock")
    );
    // v0.8.5: every variant carries the owning group id so retrieval can roll
    // the 3 colours up to one carousel card via COALESCE(product_group_id, id).
    assert_eq!(
        count(
            SHOPIFY_PRODUCT_GROUP,
            "MATCH (p:Product) WHERE p.product_group_id = 'QZC007' RETURN p.name"
        ),
        3
    );
}

// ── v0.8.5: a plain Product (no ProductGroup wrapper) carries NO
// product_group_id — every store without a variant model (doni, plain detail
// pages) leaves the prop absent so the COALESCE group-by is a no-op there. ────
const PLAIN_PRODUCT_NO_GROUP: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://shop.example/products/solo">
<link rel="canonical" href="https://shop.example/products/solo">
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"單一商品","sku":"SOLO-1",
 "image":"https://cdn/solo.jpg",
 "offers":{"@type":"Offer","price":"299","priceCurrency":"TWD"}}
</script></head><body></body></html>"#;

#[test]
fn test_plain_product_has_no_group_id() {
    // The product exists...
    assert_eq!(count(PLAIN_PRODUCT_NO_GROUP, "MATCH (p:Product) RETURN p.name"), 1);
    // ...but carries no product_group_id (prop absent → filtered out).
    assert_eq!(
        count(
            PLAIN_PRODUCT_NO_GROUP,
            "MATCH (p:Product) WHERE p.product_group_id = 'SOLO-1' RETURN p.name"
        ),
        0
    );
}

// ── Portaly: a __NEXT_DATA__ `products` keyed as a DICT (productId -> object),
// alongside a clean JSON-LD Product. Pre-fix the dict-shaped products either
// went unhandled or a parse path nuked the build. Now the clean JSON-LD wins
// (layer 0) and the build is never empty. ────────────────────────────────────
const PORTALY_DICT_PRODUCTS: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://portaly.cc/shop/x">
<link rel="canonical" href="https://portaly.cc/shop/x">
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"香氛蠟燭",
 "image":"https://cdn/candle.jpg",
 "offers":{"@type":"Offer","price":"990","priceCurrency":"TWD"}}
</script>
<script id="__NEXT_DATA__" type="application/json">
{"props":{"pageProps":{"data":{"products":{
 "xgz123":{"id":"xgz123","name":"香氛蠟燭","price":990,"image":"https://cdn/candle.jpg"}}}}}}
</script></head><body><p>NT$990</p></body></html>"#;

#[test]
fn test_portaly_dict_products_jsonld_survives_non_empty() {
    // Build is non-empty (no nuke) and the clean JSON-LD product is present.
    assert!(count(PORTALY_DICT_PRODUCTS, "MATCH (p:Product) RETURN p.name") >= 1);
    assert_eq!(
        one_str(PORTALY_DICT_PRODUCTS, "MATCH (p:Product) RETURN p.price").as_deref(),
        Some("990")
    );
}

// ── Portaly secondary coverage: the dict-keyed products parse path itself,
// exercised WITHOUT any JSON-LD so the next_data dict branch is the source.
// A productId-keyed `products` dict must yield products (not be skipped). ─────
const NEXTDATA_DICT_NO_LD: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://portaly.cc/shop/y">
<script id="__NEXT_DATA__" type="application/json">
{"props":{"pageProps":{"data":{"products":{
 "abc":{"id":"abc","name":"商品甲","price":120},
 "def":{"id":"def","name":"商品乙","price":340}}}}}}
</script></head><body><p>NT$120</p><p>NT$340</p></body></html>"#;

#[test]
fn test_nextdata_dict_keyed_products_are_collected() {
    assert_eq!(count(NEXTDATA_DICT_NO_LD, "MATCH (p:Product) RETURN p.name"), 2);
}

// ── SUPER LANDING (Hypernova): products live in a <script
// data-hypernova-key="landingdesktopApp"> whose body is HTML-comment-wrapped
// JSON (props.page.products[]). The key also appears on a <div> mount point —
// only the <script> body must be parsed. Each product: id/title/price/image +
// imageDesc1..5 gallery; props.page.url is the canonical for every product. ───
const SUPER_LANDING: &str = r#"<!doctype html><html><head></head><body>
<div data-hypernova-key="landingdesktopApp"></div>
<script type="application/json" data-hypernova-key="landingdesktopApp"><!--{"props":{"page":{"url":"https://www.shareco.me/share_perfume","products":[
 {"id":64095,"title":"極晝香水","price":2180,"originalPrice":0,
  "image":"https://cdn/super/a.jpg","imageDesc1":"https://cdn/super/a2.jpg","imageDesc2":"https://cdn/super/a3.jpg"},
 {"id":64096,"title":"極夜香水","price":1980,"originalPrice":0,
  "image":"https://cdn/super/b.jpg"}
]}}}--></script>
</body></html>"#;

#[test]
fn test_super_landing_hypernova_extracts_products() {
    // Both products (from props.page.products, not the div mount).
    assert_eq!(count(SUPER_LANDING, "MATCH (p:Product) RETURN p.name"), 2);
    // Price + canonical url stamped from props.page.url.
    let q = "MATCH (p:Product) WHERE p.name = '極晝香水' RETURN ";
    assert_eq!(
        one_str(SUPER_LANDING, &format!("{q}p.price")).as_deref(),
        Some("2180")
    );
    assert_eq!(
        one_str(SUPER_LANDING, &format!("{q}p.url")).as_deref(),
        Some("https://www.shareco.me/share_perfume")
    );
    assert_eq!(
        one_str(SUPER_LANDING, &format!("{q}p.image")).as_deref(),
        Some("https://cdn/super/a.jpg")
    );
    // Multi-image: image + imageDesc1..5 -> a gallery of >1.
    let r = rows(SUPER_LANDING, &format!("{q}p.images"));
    let imgs = r.into_iter().next().and_then(|row| row.into_iter().next());
    match imgs {
        Some(Value::List(a)) => assert_eq!(a.len(), 3),
        other => panic!("expected 3-image gallery, got {other:?}"),
    }
}

// ── doni-parity guard: a platform_json-only page with NO JSON-LD must route to
// platform_json UNCHANGED — the JSON-LD-first arm's substring pre-gate
// (html.contains("application/ld+json")) is absent, so the early DOM parse is
// skipped entirely and the product is captured exactly as before. ─────────────
const PLATFORM_ONLY_NO_LD: &str = r#"<!doctype html><html><head>
<meta property="og:url" content="https://shop.easy.co/products/tee"></head><body>
<script>var d={"products":[{"id":7,"title":"純棉上衣","price":"490","url":"/products/tee",
 "featured_image":{"img_url":"https://cdn/tee.jpg"}}]};</script>
</body></html>"#;

#[test]
fn test_doni_platform_json_only_routes_to_platform_unchanged() {
    assert_eq!(count(PLATFORM_ONLY_NO_LD, "MATCH (p:Product) RETURN p.name"), 1);
    assert_eq!(
        one_str(PLATFORM_ONLY_NO_LD, "MATCH (p:Product) RETURN p.price").as_deref(),
        Some("490")
    );
    assert_eq!(
        one_str(PLATFORM_ONLY_NO_LD, "MATCH (p:Product) RETURN p.image").as_deref(),
        Some("https://cdn/tee.jpg")
    );
}
