//! Price normalization, calibrated against the page's *visible* prices.
//!
//! Two concerns, deliberately decoupled:
//!
//! 1. **Unit detection** — purely numeric, zero text dependency, globally
//!    portable. The JSON may ship a price as whole units (`790`) or minor units
//!    /cents (`79000`); we infer which by dividing against numbers the page
//!    actually renders. We NEVER key off locale words like "元"/"NT$" — those
//!    are region-specific and ambiguous. The anchor for "this is a displayed
//!    price" is the number's *format* (a decimal-formatted amount), not a
//!    currency symbol.
//!
//! 2. **Currency detection** — best-effort, only from UNAMBIGUOUS signals (an
//!    ISO 4217 code, or a symbol that maps to exactly one currency). Ambiguous
//!    symbols ($, ¥, 元) are left undetermined for an upstream caller to set;
//!    the number itself carries no currency, so we don't guess.

use serde_json::Value;
use std::collections::HashSet;

/// Normalized price plus a confidence score, summed from independent signals
/// (no single signal is trusted alone — the currency-symbol table can never be
/// 100% complete, so unit correctness must not hinge on it):
///   +3  JSON cross-field corroboration (price vs variant.price ratio = 100) —
///       symbol-table-INDEPENDENT, the strongest signal.
///   +2  a page anchor (currency-adjacent number) matches the value.
///   +1  the matched value recurs across the page (multiple occurrences).
/// `confident` is `score >= 2`. `cents`/`currency` may still be filled at lower
/// scores (best-effort, flagged) so downstream can choose to trust or skip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceVerdict {
    pub cents: i64,
    pub currency: &'static str,
    pub score: i32,
}

impl PriceVerdict {
    pub fn confident(&self) -> bool {
        self.score >= 2
    }
}

/// Calibration context for one page. Built once, then consulted per JSON number.
pub struct PriceScale {
    /// Whole-unit amounts seen next to a currency symbol on the page.
    unit_set: HashSet<u64>,
    /// Values that appeared as anchors more than once (recurrence signal).
    recurring: HashSet<u64>,
    has_anchors: bool,
    /// ISO currency code if unambiguously determinable for the page, else "".
    currency: &'static str,
}

impl PriceScale {
    pub fn from_html(html: &str) -> Self {
        // Currency-symbol anchors only make sense in RENDERED text — the prices
        // a human sees. Scanning the whole page wastes work (on doni, <script>
        // is 78% of 245KB) and invites false anchors from JS/CSS literals
        // (`setTimeout(x, 2000)`, pixel sizes). Strip script/style/tags first;
        // the products-JSON region is handled separately by find_products_array,
        // so dropping <script> here doesn't lose the structured prices.
        let visible = visible_text(html);
        let counts = scan_displayed_amounts(&visible);
        let recurring: HashSet<u64> = counts
            .iter()
            .filter(|(_, &c)| c > 1)
            .map(|(&v, _)| v)
            .collect();
        let unit_set: HashSet<u64> = counts.into_keys().collect();
        let has_anchors = !unit_set.is_empty();
        // Currency code may live in a JSON field (priceCurrency), so detect it
        // against the full HTML, not just the rendered text.
        let currency = detect_currency(html);
        PriceScale {
            unit_set,
            recurring,
            has_anchors,
            currency,
        }
    }

    /// Score one JSON price into cents. `peer_whole` is an optional companion
    /// value KNOWN to be in whole units (e.g. a sibling `price_min` string like
    /// "790.0", or a product-level price) used for symbol-independent cross-
    /// field corroboration. Signals accumulate into `PriceVerdict::score`.
    pub fn verdict(&self, field: Option<&Value>, peer_whole: Option<f64>) -> Option<PriceVerdict> {
        let n = number_of(field?)?;
        let mut score = 0;

        // Signal 1 (+3): JSON cross-field. If a peer is known to be whole units
        // and this value is that peer ×100 (or ×1), the unit is pinned WITHOUT
        // any page symbol. Strongest, symbol-table-independent.
        let mut cents = None;
        if let Some(peer) = peer_whole.filter(|&p| p > 0.0) {
            let r = n / peer;
            if (r - 100.0).abs() < 0.5 {
                cents = Some(n.round() as i64); // this value is in cents
                score += 3;
            } else if (r - 1.0).abs() < 0.01 {
                cents = Some((n * 100.0).round() as i64); // whole units
                score += 3;
            }
        }

        // Signal 2 (+2): page anchor (currency-adjacent number) matches.
        if cents.is_none() && self.has_anchors {
            let rounded = n.round() as u64;
            if self.unit_set.contains(&rounded) {
                cents = Some((n * 100.0).round() as i64);
                score += 2;
                if self.recurring.contains(&rounded) {
                    score += 1; // Signal 3: recurs across the page
                }
            } else if n >= 100.0
                && (n / 100.0).fract() == 0.0
                && self.unit_set.contains(&((n / 100.0).round() as u64))
            {
                cents = Some(n.round() as i64);
                score += 2;
                if self.recurring.contains(&((n / 100.0).round() as u64)) {
                    score += 1;
                }
            }
        }

        // Fallback: no corroboration → whole-units assumption, score stays low.
        let cents = cents.unwrap_or_else(|| (n * 100.0).round() as i64);
        Some(PriceVerdict {
            cents,
            currency: self.currency,
            score,
        })
    }
}

/// Numeric value of a JSON price field. Strips any non-numeric characters
/// (currency symbols, separators) WITHOUT interpreting them — we only want the
/// magnitude here; the unit is decided later against the page.
fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let cleaned: String = s
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            cleaned.parse().ok()
        }
        _ => None,
    }
}

/// Extract rendered text: drop `<script>`/`<style>` bodies entirely and strip
/// all tags, keeping only what a human reads. Single pass, no regex. Used to
/// confine currency-symbol scanning to the ~6% of the page that is real text.
fn visible_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 8);
    let bytes = html.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'<' {
            // Skip <script>...</script> and <style>...</style> bodies wholesale.
            if let Some(close) = skip_block(bytes, i, b"script") {
                i = close;
                continue;
            }
            if let Some(close) = skip_block(bytes, i, b"style") {
                i = close;
                continue;
            }
            // Otherwise skip just this tag, emit a space as a token boundary.
            while i < n && bytes[i] != b'>' {
                i += 1;
            }
            i += 1; // past '>'
            out.push(' ');
        } else {
            // copy one full UTF-8 char
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&html[i..(i + ch_len).min(n)]);
            i += ch_len;
        }
    }
    out
}

/// If an opening `<tag` starts at `at`, return the index just past its matching
/// `</tag>`. Case-insensitive on the tag name.
fn skip_block(b: &[u8], at: usize, tag: &[u8]) -> Option<usize> {
    let after = at + 1;
    if after + tag.len() > b.len() || !b[after..after + tag.len()].eq_ignore_ascii_case(tag) {
        return None;
    }
    // find end of the opening tag
    let mut i = after + tag.len();
    while i < b.len() && b[i] != b'>' {
        i += 1;
    }
    i += 1;
    // scan for the closing </tag>
    while i < b.len() {
        if b[i] == b'<' && i + 1 < b.len() && b[i + 1] == b'/' {
            let name_start = i + 2;
            if name_start + tag.len() <= b.len()
                && b[name_start..name_start + tag.len()].eq_ignore_ascii_case(tag)
            {
                let mut j = name_start + tag.len();
                while j < b.len() && b[j] != b'>' {
                    j += 1;
                }
                return Some((j + 1).min(b.len()));
            }
        }
        i += 1;
    }
    Some(b.len()) // unterminated block: consume to end
}

#[inline]
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Scan the page for amounts a human sees, anchored by a CURRENCY SYMBOL
/// adjacent to a number — symbol may precede (`$790`, `₩79000`, `€19,99`) or
/// follow (`790元`, `7900円`, `790 TWD-less locales`). We don't hardcode locale
/// words; "is this a currency symbol" is decided by Unicode range (`is_currency_
/// symbol`), so $ ¥ ₩ € ₪ 元 円 ﷼ … are all covered, with or without decimals
/// (TW/JP/KR routinely show integer prices). This fixes the earlier bug of
/// requiring a `.dd` fraction, which found ZERO anchors on integer-price locales.
///
/// Three signals raise confidence per the design: (1) symbol adjacency,
/// (2) the same value recurring across the page / multiple occurrences,
/// (3) excluding "too common" numbers (years, small counts, page indices) that
/// are likely not prices. Returns whole-unit values seen at least once with a
/// currency neighbor.
fn scan_displayed_amounts(html: &str) -> std::collections::HashMap<u64, u32> {
    // Count occurrences of each currency-adjacent number; require the value to
    // be "price-like" (not a common non-price integer).
    let mut counts: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
    let chars: Vec<char> = html.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i].is_ascii_digit() {
            // consume a number token (digits + grouping/decimal separators)
            let start = i;
            while i < n && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '.') {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            // nearest non-space neighbor on each side
            let before = (0..start).rev().find_map(|k| {
                let c = chars[k];
                if c.is_whitespace() {
                    None
                } else {
                    Some(c)
                }
            });
            let after = (i..n).find_map(|k| {
                let c = chars[k];
                if c.is_whitespace() {
                    None
                } else {
                    Some(c)
                }
            });
            let has_currency_neighbor =
                before.is_some_and(is_currency_symbol) || after.is_some_and(is_currency_symbol);
            if has_currency_neighbor {
                if let Some(whole) = whole_units(&token) {
                    if is_price_like(whole) {
                        *counts.entry(whole).or_insert(0) += 1;
                    }
                }
            }
            continue;
        }
        i += 1;
    }
    counts
}

/// True for Unicode currency symbols (category Sc) plus the CJK currency words
/// that act as symbols (元 圆 圓 円 원). Covers the dedicated Unicode currency
/// block U+20A0–U+20BF and the common standalone symbols, without hardcoding
/// any locale-specific spelling.
fn is_currency_symbol(c: char) -> bool {
    // Latin-1 currency symbols + the dedicated Currency Symbols block
    // (U+20A0..U+20BF: ₠ ₡ … ₩ ₪ ₫ € ₹ ₽ ₺ ₾ ₿) + CJK currency words that
    // function as symbols. No locale-specific spelling is hardcoded.
    matches!(c, '$' | '¢' | '£' | '¤' | '¥' | '﷼')
        || ('\u{20A0}'..='\u{20BF}').contains(&c)
        || matches!(c, '元' | '圆' | '圓' | '円' | '원')
}

/// Parse a number token (`1,990.00`, `79000`, `19,99`) to whole units. A
/// trailing 2-digit group after the final separator is treated as a fractional
/// part and dropped; otherwise the value is integral.
fn whole_units(token: &str) -> Option<u64> {
    let digits: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    // If the token ends in a separator + exactly 2 digits, those are cents.
    let bytes = token.as_bytes();
    let tail_is_fraction = token.len() >= 3
        && (bytes[token.len() - 3] == b'.' || bytes[token.len() - 3] == b',')
        && bytes[token.len() - 2].is_ascii_digit()
        && bytes[token.len() - 1].is_ascii_digit();
    let whole_digits = if tail_is_fraction {
        &digits[..digits.len() - 2]
    } else {
        &digits[..]
    };
    whole_digits.parse::<u64>().ok().filter(|&v| v > 0)
}

/// Exclude "too common" integers that are usually NOT prices: tiny counts,
/// plausible years, and round page/index numbers. A real price is a specific,
/// less-common magnitude — this guards against calibrating on `1`, `2024`, etc.
fn is_price_like(v: u64) -> bool {
    v >= 10                       // tiny counts / ratings / quantities
        && !(1900..=2100).contains(&v) // plausible calendar years
}

/// Determine currency only from unambiguous signals. ISO 4217 codes appearing
/// as JSON values (`"priceCurrency":"TWD"`, `"currency":"USD"`) are trusted;
/// symbols are used ONLY when they map to exactly one currency. Ambiguous
/// symbols ($ ¥ 元) and locale words are never used — returns "" instead.
fn detect_currency(html: &str) -> &'static str {
    // 1. explicit ISO code in a currency field
    for key in ["\"priceCurrency\"", "\"currency_code\"", "\"currency\""] {
        if let Some(code) = iso_code_after(html, key) {
            return code;
        }
    }
    // 2. unambiguous symbols only
    if html.contains('€') {
        "EUR"
    } else if html.contains('£') {
        "GBP"
    } else if html.contains('₪') {
        "ILS"
    } else if html.contains('₹') {
        "INR"
    } else if html.contains('₩') {
        "KRW"
    } else {
        "" // $, ¥, 元 etc. are ambiguous → leave for upstream to set
    }
}

/// Read an ISO-4217 code (3 uppercase letters) appearing shortly after `key`.
fn iso_code_after(html: &str, key: &str) -> Option<&'static str> {
    let at = html.find(key)? + key.len();
    let tail = &html[at..(at + 16).min(html.len())];
    // find first run of 3 consecutive uppercase ASCII letters
    let bytes = tail.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i].is_ascii_uppercase()
            && bytes[i + 1].is_ascii_uppercase()
            && bytes[i + 2].is_ascii_uppercase()
        {
            return iso4217(&tail[i..i + 3]);
        }
        i += 1;
    }
    None
}

/// Intern a recognized ISO 4217 code to a static str. Unknown codes → None
/// (we only store codes we can vouch for; extend as needed).
fn iso4217(code: &str) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "USD", "EUR", "GBP", "JPY", "CNY", "TWD", "HKD", "SGD", "AUD", "CAD", "KRW", "INR", "ILS",
        "THB", "MYR", "PHP", "IDR", "VND", "BRL", "MXN",
    ];
    CODES.iter().copied().find(|&c| c == code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Anchor = number adjacent to a currency symbol, decimals or not. Mirrors
    /// doni (product.price=790 whole, variant.price=79000 cents coexisting).
    #[test]
    fn unit_detect_currency_adjacent_decimal() {
        let scale = PriceScale::from_html("<span>NT$ 790.00</span> ... 590.00 ...");
        let v = scale.verdict(Some(&json!(790)), None).unwrap();
        assert_eq!((v.cents, v.confident()), (79000, true));
        let v2 = scale.verdict(Some(&json!(79000)), None).unwrap();
        assert_eq!((v2.cents, v2.confident()), (79000, true));
    }

    /// Integer-price locales (TW/JP/KR) render NO decimals. Symbol before
    /// (`$790`, `₩79000`) or after (`790元`) must still anchor.
    #[test]
    fn unit_detect_integer_prices_prefix_and_suffix() {
        let pre = PriceScale::from_html("價格 $790 限時");
        assert_eq!(pre.verdict(Some(&json!(790)), None).unwrap().cents, 79000);

        let suf = PriceScale::from_html("售價 790元 起");
        assert_eq!(suf.verdict(Some(&json!(790)), None).unwrap().cents, 79000);

        let krw = PriceScale::from_html("₩79000 세일");
        let v = krw.verdict(Some(&json!(79000)), None).unwrap();
        assert_eq!((v.cents, v.currency), (7900000, "KRW"));
    }

    /// Cross-field corroboration needs NO page symbol: a whole-unit peer (790)
    /// pins variant.price=79000 as cents purely by the ×100 ratio. This is the
    /// guarantee that survives an incomplete currency-symbol table.
    #[test]
    fn unit_detect_json_cross_field_no_symbol() {
        let scale = PriceScale::from_html("<p>no currency symbols anywhere</p>");
        assert!(!scale.has_anchors);
        // variant 79000 against whole-unit peer 790 → cents, high score
        let v = scale.verdict(Some(&json!(79000)), Some(790.0)).unwrap();
        assert_eq!(v.cents, 79000);
        assert!(v.confident()); // score 3 from cross-field alone
                                // whole-unit value against same peer → ×100
        let w = scale.verdict(Some(&json!(790)), Some(790.0)).unwrap();
        assert_eq!((w.cents, w.confident()), (79000, true));
    }

    /// "Too common" numbers (years, tiny counts) must not become anchors.
    #[test]
    fn excludes_common_non_price_numbers() {
        let scale = PriceScale::from_html("© 2024 ¥5 顆 ★4 評價");
        assert!(!scale.has_anchors);
    }

    /// Numbers inside <script>/<style> must NOT become price anchors, even when
    /// a currency-looking char sits next to them. Confines scanning to rendered
    /// text; guards against JS/CSS numeric noise.
    #[test]
    fn script_and_style_numbers_are_not_anchors() {
        let html = r#"
            <style>.p{width:1200px;$margin:300px}</style>
            <script>setTimeout(fn,2000); var price=$990;</script>
            <span>NT$ 450</span>
        "#;
        let scale = PriceScale::from_html(html);
        // 990/1200/300/2000 are in script/style → not anchors; 450 (rendered) is.
        assert!(scale.unit_set.contains(&450));
        assert!(!scale.unit_set.contains(&990));
        assert!(!scale.unit_set.contains(&1200));
    }

    /// Ambiguous symbols ($, 元) must NOT set a currency.
    #[test]
    fn ambiguous_symbol_leaves_currency_empty() {
        let html = "NT$ 790.00 售價 590.00 元";
        assert_eq!(PriceScale::from_html(html).currency, "");
    }

    /// An explicit ISO code is trusted.
    #[test]
    fn iso_code_sets_currency() {
        let html = r#"{"price":19.99,"currency":"USD"} 19.99"#;
        assert_eq!(PriceScale::from_html(html).currency, "USD");
        let eur = "price 19.99 € shown";
        assert_eq!(PriceScale::from_html(eur).currency, "EUR");
    }

    /// No displayed anchor → whole-units assumption, flagged low-confidence.
    #[test]
    fn no_anchor_low_confidence() {
        let scale = PriceScale::from_html("<p>no prices here</p>");
        let v = scale.verdict(Some(&json!(50)), None).unwrap();
        assert_eq!((v.cents, v.confident()), (5000, false));
    }
}
