//! Price normalization — the real-world dirty work. Stores never agree on
//! units, type, or currency, and (as observed on doni easy.co) a SINGLE site
//! mixes them across nesting levels:
//!   product.price = 990        (whole units, int)
//!   product.price_min = "990.0" (whole units, string with decimal)
//!   variant.price = 99000      (cents, ×100 — Shopify convention)
//!
//! Output is always `(cents: i64, currency)` so Cypher `WHERE v.price_cents < N`
//! is unit-consistent regardless of source quirks.

use serde_json::Value;

/// Where in the platform JSON a price was read — determines the unit heuristic.
#[derive(Clone, Copy)]
pub enum PriceLevel {
    /// product.price — platforms show this in whole units more often than not,
    /// but trust an explicit decimal string (`"990.0"`) as the deciding signal.
    Product,
    /// variant.price — Shopify-like stores emit integer cents here.
    Variant,
}

/// Normalize a price field to integer cents. Returns None when the field is
/// absent or unparseable (a missing price is not zero — don't fabricate one).
pub fn to_cents(field: Option<&Value>, level: PriceLevel) -> Option<i64> {
    let raw = field?;
    match raw {
        // Integer JSON number: unit depends on level.
        Value::Number(n) if n.is_i64() => {
            let v = n.as_i64()?;
            Some(match level {
                PriceLevel::Variant => v,        // already cents
                PriceLevel::Product => v * 100,  // whole units → cents
            })
        }
        // Float number: a decimal point means whole units (e.g. 990.0).
        Value::Number(n) => Some((n.as_f64()? * 100.0).round() as i64),
        // String: strip currency symbols / thousands separators, then parse.
        // A decimal point in the string means whole units regardless of level.
        Value::String(s) => parse_money_string(s),
        _ => None,
    }
}

/// Parse a human-formatted money string to cents: "NT$1,990" → 199000,
/// "990.0" → 99000, "990" → 99000. Strips anything that isn't a digit, dot,
/// or minus. A decimal point is treated as a whole-unit separator.
fn parse_money_string(s: &str) -> Option<i64> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.contains('.') {
        let units: f64 = cleaned.parse().ok()?;
        Some((units * 100.0).round() as i64)
    } else {
        let units: i64 = cleaned.parse().ok()?;
        Some(units * 100)
    }
}

/// Resolve currency for a product object. Priority: schema.org `priceCurrency`,
/// then a `money_format` / symbol hint on the object or page, then the caller's
/// site default. Returns an ISO-4217-ish code where determinable.
pub fn resolve_currency(obj: &Value, site_default: &str) -> String {
    if let Some(c) = obj.get("priceCurrency").and_then(Value::as_str) {
        return c.to_string();
    }
    if let Some(fmt) = obj.get("money_format").and_then(Value::as_str) {
        if let Some(code) = currency_from_symbol(fmt) {
            return code.to_string();
        }
    }
    site_default.to_string()
}

/// Map a currency symbol / format string to a code. Covers the symbols seen in
/// real fixtures; extend as new stores surface.
fn currency_from_symbol(s: &str) -> Option<&'static str> {
    if s.contains("NT$") || s.contains("TWD") {
        Some("TWD")
    } else if s.contains('₪') || s.contains("ILS") {
        Some("ILS")
    } else if s.contains("US$") || s.contains("USD") {
        Some("USD")
    } else if s.contains('€') || s.contains("EUR") {
        Some("EUR")
    } else {
        None
    }
}
