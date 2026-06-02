//! Cross-extractor normalization for fields that every source carries but in a
//! different shape: a possibly-relative product/image URL, and an image that
//! hides under a platform-specific key (`featured_image.img_url`, `images[0]`,
//! a bare `image` string, …).
//!
//! Centralised here so all four extractors (platform_json / microdata /
//! dom_attr / next_data) absolutize URLs and surface `image` the SAME way —
//! the alternative (each extractor doing its own) drifted: microdata
//! absolutized + read `image`, platform_json did neither, so a product from a
//! storefront's inline JSON reached a LINE carousel with a relative
//! `/products/…` uri (400s the whole message) and no thumbnail.

use serde_json::Value;

/// Resolve a possibly-relative URL against a page origin (`scheme://host`).
///
/// Absolute (`http…`) passes through; protocol-relative `//cdn/…` borrows the
/// origin's scheme; root-relative `/products/…` gets the full origin prefixed.
/// Anything without a usable origin is returned unchanged rather than guessed.
/// Each page absolutizes against its OWN origin, so a multi-origin graph never
/// cross-attributes one shop's path onto another's host.
pub fn absolutize_url(value: &str, origin: Option<&str>) -> String {
    if value.starts_with("http") {
        return value.to_string();
    }
    let Some(origin) = origin else {
        return value.to_string();
    };
    if let Some(rest) = value.strip_prefix("//") {
        let scheme = origin.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    if value.starts_with('/') {
        if let Some((scheme, after)) = origin.split_once("://") {
            let host = after.split('/').next().unwrap_or("");
            if !host.is_empty() {
                return format!("{scheme}://{host}{value}");
            }
        }
    }
    value.to_string()
}

/// Reduce a full URL to its `scheme://host[:port]` origin. `None` for a
/// non-absolute input (a relative URL can't supply an origin).
pub fn url_origin(url: &str) -> Option<String> {
    let (scheme, after) = url.split_once("://")?;
    let host = after.split('/').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(format!("{scheme}://{host}"))
    }
}

/// One usable image URL out of an image node — a bare string, an
/// `{img_url|src|url}` object, or the first resolvable element of an array
/// (schema.org JSON-LD ships `image` as a URL-string array; storefronts ship
/// `images[]` as image objects).
fn from_image_node(node: &Value) -> Option<String> {
    match node {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(m) => m
            .get("img_url")
            .or_else(|| m.get("src"))
            .or_else(|| m.get("url"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Value::Array(a) => a.iter().find_map(from_image_node),
        _ => None,
    }
}

/// Pull a usable image URL out of a source product object, trying the shapes
/// storefronts actually emit, in priority order:
///   `featured_image.img_url` / `.src` → `image` (string, `{img_url|src}`, or
///   an `[url, …]` array) → first of `images[]` (string or object). Returns the
/// raw value; the caller absolutizes it (CDN URLs are usually already absolute,
/// but a store-relative `/i/x.jpg` shouldn't slip through).
pub fn extract_image(obj: &Value) -> Option<String> {
    if let Some(img) = obj.get("featured_image").and_then(from_image_node) {
        return Some(img);
    }
    if let Some(img) = obj.get("image").and_then(from_image_node) {
        return Some(img);
    }
    obj.get("images")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find_map(from_image_node))
}

/// A URL that is plainly chrome, not a product photo — a logo, banner, icon,
/// sprite, favicon, theme asset, or an SVG (vector UI art, never a product
/// shot). A programmatic, LLM-free guard: it never inspects pixels, only the
/// URL path. Conservative by design — it excludes only well-known non-product
/// path tokens, so a real product image is never dropped. The primary defence
/// is still the SOURCE: `extract_images` only reads a product object's own
/// `images[]`, where storefront chrome doesn't appear; this catches the rare
/// store that inlines a placeholder/badge into that array.
fn is_junk_image_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    path.ends_with(".svg")
        || [
            "/logo",
            "/banner",
            "/icon",
            "favicon",
            "/sprite",
            "placeholder",
            "/theme_src/",
            "/default_img/",
            "no-image",
            "noimage",
        ]
        .iter()
        .any(|frag| path.contains(frag))
}

/// All usable image URLs for a product, deduped and order-preserving:
/// `featured_image` first (the hero shot), then every `images[]` / `image[]`
/// element. Storefronts list the gallery under `images[]` (doni/easy.co:
/// `[{img_url,…}, …]`); schema.org JSON-LD under a singular `image` array.
/// Returns an empty vec when the object carries no image — the caller writes
/// `p.images` only when non-empty, leaving the single `p.image` as the
/// always-present primary.
pub fn extract_images(obj: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |url: Option<String>| {
        if let Some(u) = url {
            if !out.contains(&u) && !is_junk_image_url(&u) {
                out.push(u);
            }
        }
    };
    push(obj.get("featured_image").and_then(from_image_node));
    for key in ["images", "image"] {
        if let Some(arr) = obj.get(key).and_then(Value::as_array) {
            for node in arr {
                push(from_image_node(node));
            }
        } else if let Some(node) = obj.get(key) {
            // `image` may be a bare string / object (not an array).
            push(from_image_node(node));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absolutize_handles_relative_forms() {
        let o = Some("https://shop.example/collections/x");
        assert_eq!(
            absolutize_url("/products/foo", o),
            "https://shop.example/products/foo"
        );
        assert_eq!(absolutize_url("//cdn.x/i.jpg", o), "https://cdn.x/i.jpg");
        assert_eq!(absolutize_url("https://other/p", o), "https://other/p"); // absolute untouched
        assert_eq!(absolutize_url("/x", None), "/x"); // no origin → unchanged
    }

    #[test]
    fn origin_strips_to_scheme_host() {
        assert_eq!(
            url_origin("https://a.com/products/x").as_deref(),
            Some("https://a.com")
        );
        assert_eq!(url_origin("/products/x"), None);
    }

    #[test]
    fn image_from_featured_image_object() {
        let obj = json!({"featured_image": {"img_url": "https://cdn/x.jpg", "alt": "a"}});
        assert_eq!(extract_image(&obj).as_deref(), Some("https://cdn/x.jpg"));
    }

    #[test]
    fn extract_images_collects_gallery_deduped() {
        let obj = json!({
            "featured_image": {"img_url": "https://cdn/hero.jpg"},
            "images": [{"img_url": "https://cdn/hero.jpg"}, {"img_url": "https://cdn/back.jpg"}]
        });
        assert_eq!(
            extract_images(&obj),
            vec!["https://cdn/hero.jpg", "https://cdn/back.jpg"]
        );
    }

    #[test]
    fn extract_images_drops_junk_chrome() {
        let obj = json!({"images": [
            {"img_url": "https://cdn/products/real.jpg"},
            {"img_url": "https://cdn/theme_src/logo.png"},
            {"img_url": "https://cdn/badge.svg"},
            {"img_url": "https://cdn/products/back.jpg"}
        ]});
        assert_eq!(
            extract_images(&obj),
            vec![
                "https://cdn/products/real.jpg",
                "https://cdn/products/back.jpg"
            ]
        );
    }

    #[test]
    fn image_falls_back_to_images_array() {
        let obj = json!({"featured_image": null, "images": [{"img_url": "https://cdn/y.jpg"}]});
        assert_eq!(extract_image(&obj).as_deref(), Some("https://cdn/y.jpg"));
    }

    #[test]
    fn image_none_when_absent() {
        assert_eq!(extract_image(&json!({"name": "x"})), None);
    }

    #[test]
    fn image_from_singular_image_string_array() {
        // schema.org JSON-LD shape: "image":["https://…a.jpg","https://…b.png"]
        let obj = json!({"image": ["https://cdn/a.jpg", "https://cdn/b.png"]});
        assert_eq!(extract_image(&obj).as_deref(), Some("https://cdn/a.jpg"));
    }

    #[test]
    fn image_empty_array_is_none() {
        assert_eq!(extract_image(&json!({"image": []})), None);
    }
}
