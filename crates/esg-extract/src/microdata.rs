//! Extractor for schema.org microdata (`itemprop` attributes). Verified on
//! 91app (poyabuy): SSR product detail pages carry `<meta itemprop="name"
//! content="...">`, `itemprop="price"`, `itemprop="priceCurrency"` — curl
//! reachable, no JS render needed. This is the schema.org path that bypasses
//! JSON-LD entirely.

use esg_core::{GraphBuilder, NodeKind};
use scraper::{Html, Selector};
use std::sync::LazyLock;

pub struct MicrodataProduct {
    pub name: String,
    pub price: Option<String>,
    pub currency: Option<String>,
    pub image: Option<String>,
    pub url: Option<String>,
}

// Selectors compile once for the life of the process — rebuilding them per
// page was a hot-path cost when the microdata branch fires for every page.
static NAME_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[itemprop='name']").unwrap());
static PRICE_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[itemprop='price']").unwrap());
static CURRENCY_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[itemprop='priceCurrency']").unwrap());
static IMAGE_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[itemprop='image']").unwrap());
static OG_URL_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("meta[property='og:url']").unwrap());

pub fn extract_microdata_products(html: &str) -> Vec<MicrodataProduct> {
    extract_from_dom(&Html::parse_document(html))
}

/// Extract from an already-parsed DOM so a caller that parsed the page once
/// (e.g. the JSON-LD fallback on the same page) can reuse it.
///
/// schema.org microdata permits two value carriers on an `itemprop` element:
///
///   * Property-typed elements (the spec name) — `<meta itemprop="X"
///     content="Y">`, `<img itemprop="image" src="...">`, `<a itemprop="url"
///     href="...">`. The value lives on the attribute.
///   * Plain elements — `<h1 itemprop="name">Name</h1>`, `<span
///     itemprop="price">299</span>`. The value is the element's text.
///
/// Real-world storefronts mix both forms on the same page (doni / EasyStore
/// keeps `name` on `<h1>` text but `url`/`image`/`priceCurrency` on `<meta
/// content>`). The earlier "content-only" reader returned None for `name`,
/// which `extract_from_dom` treated as "no product on this page" → 0 nodes
/// for an entire shop. Falling back to the element's text covers both forms
/// without changing the contract on pages that do use `<meta>`.
pub fn extract_from_dom(doc: &Html) -> Vec<MicrodataProduct> {
    let value = |sel: &Selector| {
        doc.select(sel).find_map(|el| {
            // Property-typed carriers, in spec priority order. `content` is
            // the canonical microdata carrier (`<meta>`); `src` / `href` are
            // the implicit carriers for media / link elements. Element text
            // is the last-resort fallback for plain elements like `<h1>`.
            let v = el.value();
            v.attr("content")
                .or_else(|| v.attr("src"))
                .or_else(|| v.attr("href"))
                .map(str::to_string)
                .or_else(|| {
                    let text = el.text().collect::<String>();
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
        })
    };
    let Some(name) = value(&NAME_SEL) else {
        return Vec::new();
    };
    vec![MicrodataProduct {
        name,
        price: value(&PRICE_SEL),
        currency: value(&CURRENCY_SEL),
        image: value(&IMAGE_SEL),
        url: value(&OG_URL_SEL),
    }]
}

pub fn ingest_microdata(
    b: &mut GraphBuilder,
    products: &[MicrodataProduct],
    scale: &crate::price::PriceScale,
) {
    for p in products {
        let id = p.url.as_deref().unwrap_or(&p.name);
        let mut props = serde_json::Map::new();
        // Surface `url` into props so Cypher `RETURN p.url` works for
        // microdata-sourced Products, matching the parity contract every
        // other extractor honours (platform_json / dom_attr / next_data /
        // jsonld each serialise the whole source object whose `url` is
        // available through `read_prop`). Without this, microdata products
        // came back from Cypher with `p.url = Null`, breaking carousel
        // rendering (the card has no link target) and any downstream join
        // keyed on URL.
        if let Some(ref url) = p.url {
            props.insert("url".into(), url.clone().into());
        }
        // Run the itemprop price through the same confidence scoring as every
        // other extractor — never assume an itemprop value is trustworthy
        // (it may be cents-vs-whole ambiguous like any other source).
        let price_value = p
            .price
            .as_ref()
            .map(|s| serde_json::Value::String(s.clone()));
        if let Some(v) = scale.verdict(price_value.as_ref(), None) {
            let confident = v.confident();
            let score = v.score;
            let currency = v.currency;
            props.insert("price".into(), serde_json::Value::String(v.price));
            if !currency.is_empty() {
                props.insert("currency".into(), currency.into());
            }
            props.insert("price_confident".into(), confident.into());
            props.insert("price_score".into(), score.into());
        }
        // itemprop priceCurrency is an explicit signal; prefer it when present.
        if let Some(ref cur) = p.currency {
            props.insert("currency".into(), cur.clone().into());
        }
        if let Some(ref img) = p.image {
            props.insert("image".into(), img.clone().into());
        }
        b.upsert_node(
            NodeKind::Product,
            id,
            &p.name,
            &serde_json::Value::Object(props).to_string(),
        );
    }
}
