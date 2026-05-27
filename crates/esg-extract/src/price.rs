//! Price normalization, calibrated against the page's *visible* prices.
//!
//! The reliable signal isn't the platform's convention — it's what the page
//! renders for a human. On doni easy.co the JSON ships three values for one
//! 790-unit product (`price=790`, `price_min="790.0"`, `variant.price=79000`)
//! but the page shows `NT$ 790.00`. That visible text is ground truth: dividing
//! each JSON number by it reveals the unit (ratio 1 → whole units, 100 → cents)
//! and the symbol reveals the currency. No per-platform unit rule needed.

use serde_json::Value;
use std::collections::HashSet;

/// A price as a human sees it on the page: amount in whole units + currency.
#[derive(Debug, Clone, Copy)]
pub struct VisiblePrice {
    pub units: f64,
    pub currency: &'static str,
}

/// Calibration context for one page: every visible price found in the HTML.
/// Built once per page, then consulted to normalize each JSON number.
pub struct PriceScale {
    visible: Vec<VisiblePrice>,
    /// Distinct whole-unit amounts, for fast ratio matching.
    unit_set: HashSet<u64>,
    default_currency: &'static str,
}

impl PriceScale {
    /// Scan the HTML for rendered prices like `NT$ 790.00`, `$1,990`, `€19.99`.
    pub fn from_html(html: &str) -> Self {
        let mut visible = Vec::new();
        let mut unit_set = HashSet::new();
        // Walk char boundaries (HTML is UTF-8; CJK pages have multi-byte chars,
        // so byte-index slicing would panic mid-character).
        let mut rest = html;
        while !rest.is_empty() {
            if let Some((cur, sym_len)) = currency_at(rest) {
                if let Some((units, consumed)) = parse_amount(&rest[sym_len..]) {
                    visible.push(VisiblePrice { units, currency: cur });
                    unit_set.insert(units.round() as u64);
                    rest = &rest[sym_len + consumed..];
                    continue;
                }
            }
            // advance one full char
            let step = rest.chars().next().map(char::len_utf8).unwrap_or(1);
            rest = &rest[step..];
        }
        let default_currency = visible.first().map(|v| v.currency).unwrap_or("");
        PriceScale { visible, unit_set, default_currency }
    }

    fn has_visible(&self) -> bool {
        !self.visible.is_empty()
    }

    /// Normalize one JSON price number to integer cents, using visible prices
    /// to infer the unit. Returns `(cents, currency, confident)`.
    /// `confident=false` means no visible price matched — value kept as-is.
    pub fn to_cents(&self, field: Option<&Value>) -> Option<(i64, &'static str, bool)> {
        let n = number_of(field?)?;
        if !self.has_visible() {
            // No anchor: assume whole units (the schema.org / JSON-LD norm).
            return Some(((n * 100.0).round() as i64, self.default_currency, false));
        }
        // A JSON value equal to a visible whole-unit amount → it's whole units.
        if self.unit_set.contains(&(n.round() as u64)) {
            return Some(((n * 100.0).round() as i64, self.default_currency, true));
        }
        // A JSON value equal to visible×100 → it's cents.
        if n >= 100.0 && self.unit_set.contains(&((n / 100.0).round() as u64))
            && (n / 100.0).fract() == 0.0
        {
            return Some((n.round() as i64, self.default_currency, true));
        }
        // No match: fall back to whole-units assumption, flagged low-confidence.
        Some(((n * 100.0).round() as i64, self.default_currency, false))
    }
}

/// Numeric value of a JSON price field (number, or numeric string like "990.0"
/// / "NT$1,990"). Returns the amount in its own native unit (unknown until
/// calibrated against the page).
fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let cleaned: String =
                s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            cleaned.parse().ok()
        }
        _ => None,
    }
}

/// Detect a currency symbol/code at the start of `s`; return (code, byte_len).
fn currency_at(s: &str) -> Option<(&'static str, usize)> {
    const SYMS: &[(&str, &str)] = &[
        ("NT$", "TWD"),
        ("US$", "USD"),
        ("$", "USD"),
        ("€", "EUR"),
        ("₪", "ILS"),
        ("£", "GBP"),
        ("¥", "JPY"),
    ];
    for (sym, code) in SYMS {
        if s.starts_with(sym) {
            return Some((code, sym.len()));
        }
    }
    None
}

/// Parse `[whitespace] 1,990.00` after a currency symbol → (1990.0, byte_len).
/// Stops at the first char that isn't part of the number.
fn parse_amount(s: &str) -> Option<(f64, usize)> {
    let mut consumed = 0;
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_whitespace() && digits.is_empty() {
            consumed += c.len_utf8();
        } else if c.is_ascii_digit() || c == '.' {
            digits.push(c);
            consumed += c.len_utf8();
        } else if c == ',' {
            consumed += c.len_utf8(); // thousands separator, skip
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    let val: f64 = digits.parse().ok()?;
    Some((val, consumed))
}
