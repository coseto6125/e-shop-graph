//! Extractor for schema.org microdata (`itemprop` attributes). Verified on
//! 91app (poyabuy): SSR product detail pages carry `<meta itemprop="name"
//! content="...">`, `itemprop="price"`, `itemprop="priceCurrency"` — curl
//! reachable, no JS render needed. This is the schema.org path that bypasses
//! JSON-LD entirely.

use esg_core::{GraphBuilder, NodeKind};
use scraper::{Html, Selector};

pub struct MicrodataProduct {
    pub name: String,
    pub price: Option<String>,
    pub currency: Option<String>,
    pub image: Option<String>,
    pub url: Option<String>,
}

pub fn extract_microdata_products(html: &str) -> Vec<MicrodataProduct> {
    let doc = Html::parse_document(html);
    let name_sel = Selector::parse("[itemprop='name']").unwrap();
    let price_sel = Selector::parse("[itemprop='price']").unwrap();
    let currency_sel = Selector::parse("[itemprop='priceCurrency']").unwrap();
    let image_sel = Selector::parse("[itemprop='image']").unwrap();
    let og_url_sel = Selector::parse("meta[property='og:url']").unwrap();

    let name = doc
        .select(&name_sel)
        .find_map(|el| el.value().attr("content").map(str::to_string));
    let price = doc
        .select(&price_sel)
        .find_map(|el| el.value().attr("content").map(str::to_string));
    let currency = doc
        .select(&currency_sel)
        .find_map(|el| el.value().attr("content").map(str::to_string));
    let image = doc
        .select(&image_sel)
        .find_map(|el| el.value().attr("content").map(str::to_string));
    let url = doc
        .select(&og_url_sel)
        .find_map(|el| el.value().attr("content").map(str::to_string));

    let Some(name) = name else {
        return Vec::new();
    };

    vec![MicrodataProduct { name, price, currency, image, url }]
}

pub fn ingest_microdata(b: &mut GraphBuilder, products: &[MicrodataProduct]) {
    for p in products {
        let id = p.url.as_deref().unwrap_or(&p.name);
        let mut props = serde_json::Map::new();
        if let Some(ref price) = p.price {
            if let Ok(v) = price.parse::<f64>() {
                props.insert("price_cents".into(), ((v * 100.0).round() as i64).into());
            }
        }
        if let Some(ref cur) = p.currency {
            props.insert("currency".into(), cur.clone().into());
        }
        if let Some(ref img) = p.image {
            props.insert("image".into(), img.clone().into());
        }
        props.insert("price_confident".into(), true.into());
        b.upsert_node(
            NodeKind::Product,
            id,
            &p.name,
            &serde_json::Value::Object(props).to_string(),
        );
    }
}
