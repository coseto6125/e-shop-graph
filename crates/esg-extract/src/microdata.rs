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
pub fn extract_from_dom(doc: &Html) -> Vec<MicrodataProduct> {
    let attr = |sel: &Selector| {
        doc.select(sel)
            .find_map(|el| el.value().attr("content").map(str::to_string))
    };
    let Some(name) = attr(&NAME_SEL) else {
        return Vec::new();
    };
    vec![MicrodataProduct {
        name,
        price: attr(&PRICE_SEL),
        currency: attr(&CURRENCY_SEL),
        image: attr(&IMAGE_SEL),
        url: attr(&OG_URL_SEL),
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
        // Run the itemprop price through the same confidence scoring as every
        // other extractor — never assume an itemprop value is trustworthy
        // (it may be cents-vs-whole ambiguous like any other source).
        let price_value = p
            .price
            .as_ref()
            .map(|s| serde_json::Value::String(s.clone()));
        if let Some(v) = scale.verdict(price_value.as_ref(), None) {
            props.insert("price_cents".into(), v.cents.into());
            if !v.currency.is_empty() {
                props.insert("currency".into(), v.currency.into());
            }
            props.insert("price_confident".into(), v.confident().into());
            props.insert("price_score".into(), v.score.into());
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
