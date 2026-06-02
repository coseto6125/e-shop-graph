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
    pub description: Option<String>,
    /// The store's stable numeric product id, when the page exposes one
    /// (`data-addtocart='{"id":N,…}'` / `data-product-id` / a `"product_id":N`
    /// JS literal). It's the SAME id a listing page's product-array carries, so
    /// keying on it collapses the detail-page node and the listing-card node
    /// into one — the duplicate-Product bug that split a product's description
    /// (detail page) from its price/image (listing card). None on stores that
    /// don't surface it; the id then falls back to the url.
    pub product_id: Option<String>,
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
// Product description for a carousel subtitle. `og:description` is the clean,
// page-level marketing copy (storefronts also expose it as
// `<meta name=description>`); the JSON-LD `description` field is often raw HTML
// (froala editor markup) and unusable, so we read the og form, not itemprop.
// Page-level like og:url — only safe to attach when the page is ONE product
// (scoped.len() == 1), else product A's copy would bleed onto product B.
static OG_DESC_SEL: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("meta[property='og:description'], meta[name='description']").unwrap()
});
// Per-product container: an itemscope whose itemtype names a Product. A listing
// page renders one of these per card, so grouping by container stops one
// product's price binding to another's name.
static PRODUCT_SCOPE_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[itemscope][itemtype*='Product']").unwrap());
static URL_SEL: LazyLock<Selector> = LazyLock::new(|| Selector::parse("[itemprop='url']").unwrap());
// Store-native product id carriers. `data-addtocart='{"id":N,…}'` (the cart
// form) and `data-product-id` are how storefronts (easy.co / EasyStore) stamp
// the numeric product id onto the detail page; the SAME id appears in a listing
// page's product array, so reading it lets both views share one node id.
static ADDTOCART_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[data-addtocart]").unwrap());
static PRODUCT_ID_ATTR_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[data-product-id]").unwrap());

/// Page-level store product id, read from the DOM carriers storefronts emit.
/// `data-addtocart` holds a JSON object whose `id` is the product id; a bare
/// `data-product-id` attribute is the simpler form. None when neither is
/// present — the caller falls back to the url for identity.
fn page_product_id(doc: &Html) -> Option<String> {
    if let Some(el) = doc.select(&ADDTOCART_SEL).next() {
        if let Some(raw) = el.value().attr("data-addtocart") {
            if let Ok(serde_json::Value::Object(obj)) =
                serde_json::from_str::<serde_json::Value>(raw)
            {
                if let Some(id) = obj.get("id") {
                    // `id` is usually a JSON number; render it without quotes/decimals.
                    let s = id
                        .as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| id.as_str().map(str::to_string));
                    if let Some(s) = s {
                        if !s.is_empty() {
                            return Some(s);
                        }
                    }
                }
            }
        }
    }
    doc.select(&PRODUCT_ID_ATTR_SEL)
        .find_map(|el| el.value().attr("data-product-id").map(str::to_string))
        .filter(|s| !s.is_empty())
}

pub fn extract_microdata_products(html: &str) -> Vec<MicrodataProduct> {
    extract_from_dom(&Html::parse_document(html))
}

/// Resolve a possibly-relative URL against the page's canonical origin.
///
/// A listing card's `<a itemprop="url" href="/products/foo">` carries a
/// root-relative path, while the same product's `og:url` (and other cards)
/// carry the absolute form. Stored verbatim, the two strings become two
/// distinct node ids for one product — the duplicate-node bug that also leaks
/// a relative `/products/...` into a LINE carousel `uri`, which 400s the whole
/// message. Absolutizing against the page's own `og:url` origin collapses both
/// to one id (dedup) and guarantees every carousel link is a full URL.
///
/// `base` is the page's `og:url` (always absolute). We only need its origin —
/// `scheme://host[:port]` — which is everything up to the path. Handles the two
/// relative forms storefronts emit; anything already absolute (`http`) passes
/// through untouched, so multi-origin graphs never cross-attribute (each page
/// absolutizes against its own origin, an `a.com` page never borrows `b.com`).
fn absolutize(value: &str, base: Option<&str>) -> String {
    if value.starts_with("http") {
        return value.to_string();
    }
    let Some(base) = base else {
        return value.to_string();
    };
    // protocol-relative: `//cdn.x.com/img.jpg` → borrow the page's scheme only.
    if let Some(rest) = value.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    // root-relative: `/products/foo` → prepend the page's `scheme://host[:port]`.
    // Origin = base up to the first `/` after the `scheme://` marker.
    if value.starts_with('/') {
        if let Some((scheme, after)) = base.split_once("://") {
            let host = after.split('/').next().unwrap_or("");
            if !host.is_empty() {
                return format!("{scheme}://{host}{value}");
            }
        }
    }
    // No usable origin or an unrecognised relative form — leave as-is rather
    // than fabricate a wrong URL.
    value.to_string()
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
    // og:url / og:description are page-level (one per document), so read them
    // once and share; per-product itemprops are read within each scope.
    let page_url = doc.select(&OG_URL_SEL).find_map(itemprop_value);
    let page_desc = doc.select(&OG_DESC_SEL).find_map(itemprop_value);
    // Page-level store product id (detail page = one product). Like og:desc, only
    // safe to attach when the page is ONE product — a listing's page-level id (if
    // any) would otherwise stamp every card with the same id.
    let page_pid = page_product_id(doc);

    // Listing / category pages mark each card with a Product itemscope. Extract
    // one MicrodataProduct per container, reading each itemprop ONLY within that
    // container's subtree so product A's price can't bind to product B's name.
    let mut scoped: Vec<MicrodataProduct> = doc
        .select(&PRODUCT_SCOPE_SEL)
        .filter_map(|scope| {
            let name = scope.select(&NAME_SEL).find_map(itemprop_value)?;
            let base = page_url.as_deref();
            Some(MicrodataProduct {
                name,
                price: scope.select(&PRICE_SEL).find_map(itemprop_value),
                currency: scope.select(&CURRENCY_SEL).find_map(itemprop_value),
                image: scope
                    .select(&IMAGE_SEL)
                    .find_map(itemprop_value)
                    .map(|img| absolutize(&img, base)),
                // Per-card product URL (`itemprop=url`) so each listed product
                // has a distinct id; fall back to the page's canonical og:url.
                // A card's `href` is often root-relative (`/products/foo`) —
                // absolutize against the page origin so it dedups against the
                // absolute form instead of forking a second node.
                url: scope
                    .select(&URL_SEL)
                    .find_map(itemprop_value)
                    .map(|u| absolutize(&u, base))
                    .or_else(|| page_url.clone()),
                description: None,
                product_id: None,
            })
        })
        .collect();
    if !scoped.is_empty() {
        // og:description + page-level product id are page-level, so they describe
        // THE product only when the page is a single-product detail page. On a
        // listing (many scopes) they'd be the collection's, wrong for any one
        // card — attach only when there's exactly one scope.
        if let [only] = scoped.as_mut_slice() {
            only.description = page_desc;
            only.product_id = page_pid;
        }
        return scoped;
    }

    // No explicit Product itemscope (a single-product page that just sprinkles
    // itemprops): fall back to reading the whole document as one product.
    let Some(name) = doc.select(&NAME_SEL).find_map(itemprop_value) else {
        return Vec::new();
    };
    let base = page_url.as_deref();
    vec![MicrodataProduct {
        name,
        price: doc.select(&PRICE_SEL).find_map(itemprop_value),
        currency: doc.select(&CURRENCY_SEL).find_map(itemprop_value),
        image: doc
            .select(&IMAGE_SEL)
            .find_map(itemprop_value)
            .map(|img| absolutize(&img, base)),
        url: page_url,
        description: page_desc,
        product_id: page_pid,
    }]
}

/// Read the value an `itemprop` element carries: the `content`/`src`/`href`
/// attribute in spec priority order, else the element's trimmed text. Returns
/// None for an empty text element with no carrier attribute.
fn itemprop_value(el: scraper::ElementRef) -> Option<String> {
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
}

pub fn ingest_microdata(
    b: &mut GraphBuilder,
    products: &[MicrodataProduct],
    scale: &crate::price::PriceScale,
) {
    for p in products {
        // Identity priority: store product id → url → name. The product id is
        // the cross-view stable key (listing card + detail page carry the same
        // one), so it dedups a product that the url alone wouldn't when the two
        // views' urls differ (collection-scoped vs bare). url is the portable
        // fallback for stores that don't expose an id; name is last resort.
        let id = p
            .product_id
            .as_deref()
            .or(p.url.as_deref())
            .unwrap_or(&p.name);
        let mut props = serde_json::Map::new();
        if let Some(ref pid) = p.product_id {
            props.insert("product_id".into(), pid.clone().into());
        }
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
            v.write_into(&mut props);
        }
        // itemprop priceCurrency is an explicit signal; prefer it when present.
        if let Some(ref cur) = p.currency {
            props.insert("currency".into(), cur.clone().into());
        }
        if let Some(ref img) = p.image {
            props.insert("image".into(), img.clone().into());
        }
        // Product blurb for a carousel subtitle (og:description). Stored raw;
        // the consumer (enoract) truncates to its per-platform subtitle cap.
        if let Some(ref desc) = p.description {
            let trimmed = desc.trim();
            if !trimmed.is_empty() {
                props.insert("description".into(), trimmed.into());
            }
        }
        // Skip a node with no product signal — same multi-signal gate as the
        // JSON-LD / platform_json paths. Microdata `itemprop` is the loosest
        // source (no `itemtype=Product` required), so an easy.co blog/FAQ page
        // with `itemprop=name/url` but no price and a `/blogs/` url is mistaken
        // for a product without this; the gate drops it (its text still reaches
        // the bm25 lane) while keeping any genuinely priced/imaged item.
        if !crate::has_product_signal(&props) {
            continue;
        }
        b.upsert_node(
            NodeKind::Product,
            id,
            &p.name,
            &serde_json::Value::Object(props).to_string(),
        );
    }
}
