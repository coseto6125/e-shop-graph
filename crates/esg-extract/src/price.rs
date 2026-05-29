//! Price normalization, calibrated against the page's *visible* prices.
//!
//! Two concerns, deliberately decoupled:
//!
//! 1. **Unit detection** — purely numeric, zero text dependency, globally
//!    portable. The JSON may ship a price as whole units (`790`) or minor units
//!    /cents (`79000`); we infer which by dividing against numbers the page
//!    actually renders. Unit detection never keys off currency words — the
//!    anchor for "this is a displayed price" is the number's *format* (a
//!    decimal-formatted amount), not a currency symbol.
//!
//! 2. **Currency detection** — best-effort, only from UNAMBIGUOUS signals (an
//!    ISO 4217 code, or a symbol/notation that maps to exactly one currency,
//!    e.g. the Taiwan-specific `NT$` → TWD). Bare ambiguous symbols ($, ¥, 元)
//!    are left undetermined for an upstream caller to set; the number itself
//!    carries no currency, so we don't guess.
//!
//! The output is a single `Decimal`-backed `price` field — what the source
//! displays to a human, with currency-aware precision:
//!   * ISO 4217 zero-decimal currencies (JPY/KRW/TWD/VND/IDR): integer string
//!     (`"690"`). A JSON `690.00` and a JSON `690` both surface as `"690"`.
//!   * Two-decimal currencies (USD/EUR/GBP/…): up to two fractional digits,
//!     trailing-`.00` stripped (`"4.20"`, `"4"` — not `"4.0000"`).
//!   * Detected JSON-cents values (e.g. variants ship `4200` while the page
//!     renders `42.00 USD`): divided back into whole units before formatting.
//!
//! `Decimal` throughout — `f64` would have rounded `690.00 * 100` to
//! `68999.999…` and that has actually bitten this pipeline before.

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::HashSet;

/// ISO 4217 currencies with zero minor units. The same Decimal value is
/// already in whole units for these — no `/100` rescale and no fractional
/// digits when formatting. Sourced from
/// <https://en.wikipedia.org/wiki/ISO_4217#Active_codes_(List_One)>;
/// limited to codes esg already recognises (see `iso4217`).
const ZERO_DECIMAL_CURRENCIES: &[&str] = &["JPY", "KRW", "TWD", "VND", "IDR"];

fn is_zero_decimal(currency: &str) -> bool {
    ZERO_DECIMAL_CURRENCIES.contains(&currency)
}

/// Normalized price plus a confidence score, summed from independent signals
/// (no single signal is trusted alone — the currency-symbol table can never be
/// 100% complete, so unit correctness must not hinge on it):
///   +3  JSON cross-field corroboration (price vs variant.price ratio = 100) —
///       symbol-table-INDEPENDENT, the strongest signal.
///   +2  a page anchor (currency-adjacent number) matches the value.
///   +1  the matched value recurs across the page (multiple occurrences).
/// `confident` is `score >= 2`. `price`/`currency` may still be filled at lower
/// scores (best-effort, flagged) so downstream can choose to trust or skip.
///
/// The verdict ships TWO views of the SAME amount, derived from one `whole`
/// `Decimal` so they can never drift:
///   * `price` — the human-readable amount as a string (zero-cent trailing
///     strip, see module docs). This is the value consumers display / use.
///   * `cents` — the same amount in the currency's MINOR units as an integer
///     (TWD 690 → 690, USD 4.20 → 420), for inspection / debugging. Computed
///     as `whole × 10^scale`; never stored independently of `price`.
///
/// `scale` is the currency's minor-unit exponent (0 for zero-decimal
/// currencies like TWD/JPY, else 2) — the bridge between the two views. `f64`
/// is never used (it rounded `690.00 × 100` to `68999.999…` before).
#[derive(Debug, Clone, PartialEq)]
pub struct PriceVerdict {
    pub price: String,
    pub cents: i64,
    pub scale: u8,
    pub currency: &'static str,
    pub score: i32,
}

impl PriceVerdict {
    pub fn confident(&self) -> bool {
        self.score >= 2
    }

    /// Write every price prop this verdict carries into a props map — the ONE
    /// place that decides which keys a priced Product/Variant node gets, so
    /// the four extractors (platform_json / next_data / microdata / dom_attr)
    /// can never drift apart on the set. `currency` is omitted when empty
    /// (ambiguous symbol, no ISO code) rather than written as `""`, matching
    /// the prior per-extractor behaviour.
    pub fn write_into(&self, map: &mut serde_json::Map<String, Value>) {
        map.insert("price".into(), Value::String(self.price.clone()));
        map.insert("price_cents".into(), Value::Number(self.cents.into()));
        map.insert("price_scale".into(), Value::Number(self.scale.into()));
        if !self.currency.is_empty() {
            map.insert("currency".into(), Value::String(self.currency.to_string()));
        }
        map.insert("price_confident".into(), Value::Bool(self.confident()));
        map.insert("price_score".into(), Value::Number(self.score.into()));
    }
}

/// Minor-unit exponent for a currency: 0 for zero-decimal currencies (the
/// whole-unit value already IS the minor-unit value), else 2. Mirrors the
/// branch in `format_price` so the two stay consistent.
fn minor_unit_scale(currency: &str) -> u8 {
    if is_zero_decimal(currency) {
        0
    } else {
        2
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
    /// A no-signal scale, for pages that yielded no products — skips the
    /// full visible-text scan + currency detection that `from_html` does.
    pub fn empty() -> Self {
        PriceScale {
            unit_set: HashSet::new(),
            recurring: HashSet::new(),
            has_anchors: false,
            currency: "",
        }
    }

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

    /// Score one JSON price into a normalized whole-unit `Decimal`.
    /// `peer_whole` is an optional companion value KNOWN to be in whole units
    /// (e.g. a sibling `price_min` string like "790.0", or a product-level
    /// price) used for symbol-independent cross-field corroboration. Signals
    /// accumulate into `PriceVerdict::score`.
    ///
    /// Returns `None` when the field has no extractable magnitude. Returns a
    /// verdict whose `price` is the whole-unit string formatted per the
    /// currency's minor-unit convention.
    pub fn verdict(
        &self,
        field: Option<&Value>,
        peer_whole: Option<Decimal>,
    ) -> Option<PriceVerdict> {
        let n = decimal_of(field?)?;

        // Detect whether the JSON number was already in minor units (cents);
        // if so, divide back into whole units before formatting. `whole`
        // is the post-detection amount in whole currency units (e.g. 4.20
        // USD or 690 TWD), `score` the confidence in that decision.
        let mut whole: Option<Decimal> = None;
        let mut score = 0;

        // Signal 1 (+3): JSON cross-field. If a peer is known to be whole units
        // and this value is that peer ×100 (or ×1), the unit is pinned WITHOUT
        // any page symbol. Strongest, symbol-table-independent.
        if let Some(peer) = peer_whole.filter(|p| p.is_sign_positive() && !p.is_zero()) {
            // Use a Decimal-safe ratio: subtract instead of divide so f64
            // rounding never reaches us. r ≈ n / peer.
            // `checked_*` throughout: a crafted peer/n near Decimal::MAX overflows
            // the ×100 or the subtraction, and `Decimal`'s `*` / `-` PANIC on
            // overflow. Such magnitudes are not real prices — on overflow we
            // simply skip cross-field corroboration (score stays low) instead of
            // crashing. `is_cents` / `is_whole` are None when the math overflowed.
            let is_cents = peer
                .checked_mul(Decimal::ONE_HUNDRED)
                .and_then(|hundred_peer| n.checked_sub(hundred_peer))
                .map(|diff| diff.abs() < (peer / Decimal::TWO))
                .unwrap_or(false);
            let is_whole = n
                .checked_sub(peer)
                .map(|diff| diff.abs() < (peer / Decimal::from(100)))
                .unwrap_or(false);
            if is_cents {
                whole = Some(n / Decimal::ONE_HUNDRED); // this value is in cents
                score += 3;
            } else if is_whole {
                whole = Some(n); // already whole units
                score += 3;
            }
        }

        // Signal 2 (+2): page anchor (currency-adjacent number) matches.
        if whole.is_none() && self.has_anchors {
            let rounded_u64 = decimal_to_u64_round(&n);
            if let Some(r) = rounded_u64 {
                if self.unit_set.contains(&r) {
                    whole = Some(n);
                    score += 2;
                    if self.recurring.contains(&r) {
                        score += 1; // Signal 3: recurs across the page
                    }
                } else if n >= Decimal::ONE_HUNDRED {
                    // The JSON number could be cents (4200) while the visible
                    // whole price is /100 of it (42). Check the divided form
                    // against the anchor set.
                    let divided = n / Decimal::ONE_HUNDRED;
                    if divided.fract().is_zero() {
                        if let Some(dr) = decimal_to_u64_round(&divided) {
                            if self.unit_set.contains(&dr) {
                                whole = Some(divided);
                                score += 2;
                                if self.recurring.contains(&dr) {
                                    score += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fallback: no corroboration → whole-units assumption, score stays low.
        let whole = whole.unwrap_or(n);
        // Both views come from `whole`: the display string and the minor-unit
        // integer. `cents = whole × 10^scale`, computed in Decimal then rounded
        // to i64 — never via f64. On overflow (a magnitude no real price hits)
        // fall back to 0 cents rather than panic; `price`/`score` still stand.
        let scale = minor_unit_scale(self.currency);
        let cents = whole
            .checked_mul(Decimal::from(10u64.pow(scale as u32)))
            .map(|c| c.round())
            .and_then(|c| c.to_i64())
            .unwrap_or(0);
        Some(PriceVerdict {
            price: format_price(whole, self.currency),
            cents,
            scale,
            currency: self.currency,
            score,
        })
    }
}

/// Render a whole-unit `Decimal` per the currency's minor-unit convention:
///   * Zero-decimal currencies (JPY/KRW/TWD/…): integer string, no `.`.
///   * Two-decimal currencies (default for ambiguous / minor-unit codes):
///     up to two fractional digits, trailing zeros stripped after the
///     decimal point. `4.20` → `"4.20"`, `4.00` → `"4"`, `4.5` → `"4.50"`.
///
/// The output is what a UI displays as-is. Consumers that need arithmetic
/// parse it back via `Decimal::from_str_exact`.
fn format_price(amount: Decimal, currency: &str) -> String {
    if is_zero_decimal(currency) {
        // Round to integer (any fractional component on a zero-decimal
        // currency is presentational noise, e.g. JSON `690.00`).
        return amount.round_dp(0).trunc().to_string();
    }
    // Two-decimal default. Round to 2 dp, then drop a pure-zero fractional
    // part so `4.00` shows as `4`, but keep `4.20` as `4.20` (the user-
    // visible precision the source intended).
    let two = amount.round_dp(2);
    if two.fract().is_zero() {
        two.trunc().to_string()
    } else {
        // Format with exactly 2 fractional digits so `4.5` → `"4.50"`.
        format!("{:.2}", two)
    }
}

/// Decimal value of a JSON price field — Decimal-safe parse (never via f64).
/// Strips non-numeric characters (currency symbols, separators) WITHOUT
/// interpreting them — magnitude only; unit is decided later against the page.
fn decimal_of(v: &Value) -> Option<Decimal> {
    match v {
        Value::Number(n) => {
            // Prefer the JSON token's textual form so `690.00` round-trips
            // exactly. `Number::to_string()` reproduces the parsed digits.
            Decimal::from_str_exact(&n.to_string()).ok()
        }
        Value::String(s) => {
            let cleaned: String = s
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            Decimal::from_str_exact(&cleaned).ok()
        }
        _ => None,
    }
}

/// Rounded u64 image of a Decimal for HashSet membership tests. Returns None
/// when the value is negative or overflows u64 — both treated as "no anchor".
fn decimal_to_u64_round(d: &Decimal) -> Option<u64> {
    let rounded = d.round();
    if rounded.is_sign_negative() {
        return None;
    }
    rounded.to_u64()
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

/// Parse a number token (`1,990.00`, `79000`, `19,99`, `5.5`) to whole units.
/// The trailing group after the LAST separator is a fractional part — and is
/// dropped — when it has 1 or 2 digits (`5.5`→5, `19,99`→19, `4.20`→4); a
/// 3-digit trailing group is a thousands group, so the separator is grouping
/// and all digits are integral (`1,200`→1200, `12.345`→12345). A token with no
/// separator is integral (`79000`). This fixes the earlier "exactly 2 digits"
/// rule that left a single decimal place (`5.5`) glued on as `55`.
fn whole_units(token: &str) -> Option<u64> {
    let all_digits: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
    if all_digits.is_empty() {
        return None;
    }
    let frac_len = token
        .rfind(['.', ','])
        .map(|sep| token[sep + 1..].chars().filter(|c| c.is_ascii_digit()).count())
        .filter(|&n| n == 1 || n == 2) // 1–2 trailing digits = cents; 3 = grouping
        .unwrap_or(0);
    let whole_digits = &all_digits[..all_digits.len() - frac_len];
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
    // 2. unambiguous symbols only. `NT$` is Taiwan-specific notation (the bare
    //    `$` is ambiguous, but the `NT` prefix pins it to TWD) — check it before
    //    the bare-symbol fallthrough so a TWD shop with no ISO code (the common
    //    easy.co / cyberbiz case) gets the right zero-decimal scale instead of
    //    being treated as a 2-decimal currency (which would 100× its cents).
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
    } else if html.contains("NT$") || html.contains("NT＄") {
        "TWD"
    } else {
        "" // bare $, ¥, 元 etc. are ambiguous → leave for upstream to set
    }
}

/// Read an ISO-4217 code (3 ASCII letters) appearing shortly after `key`. The
/// letters may be lower- or mixed-case (`"currency":"usd"` is common); they are
/// upper-cased before the `iso4217` whitelist lookup, which rejects any 3-letter
/// run that isn't a recognised code — so case-insensitivity can't fabricate a
/// currency from arbitrary prose.
fn iso_code_after(html: &str, key: &str) -> Option<&'static str> {
    // `key` is ASCII so `at` lands on a char boundary, but `at + 16` may fall
    // mid-codepoint — slicing `&html[at..at+16]` there panics. Scan the raw
    // bytes of the 16-byte window directly (the code is 3 ASCII letters;
    // non-ASCII bytes fail the alphabetic test), so no string slice is taken on
    // an unaligned boundary.
    let at = html.find(key)? + key.len();
    let bytes = html.as_bytes();
    let end = (at + 16).min(bytes.len());
    let window = &bytes[at..end];
    let mut i = 0;
    while i + 3 <= window.len() {
        if window[i].is_ascii_alphabetic()
            && window[i + 1].is_ascii_alphabetic()
            && window[i + 2].is_ascii_alphabetic()
        {
            let upper = [
                window[i].to_ascii_uppercase(),
                window[i + 1].to_ascii_uppercase(),
                window[i + 2].to_ascii_uppercase(),
            ];
            // 3 ASCII letters → always valid UTF-8.
            if let Some(code) = iso4217(std::str::from_utf8(&upper).unwrap()) {
                return Some(code);
            }
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

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    /// Anchor = number adjacent to a currency symbol, decimals or not. Mirrors
    /// doni (product.price=790 whole, variant.price=79000 cents coexisting).
    /// `NT$` pins the currency to TWD (zero-decimal), so 790 → price "790",
    /// cents 790, scale 0 — NOT the 79000 a 2-decimal default would give.
    #[test]
    fn unit_detect_currency_adjacent_decimal() {
        let scale = PriceScale::from_html("<span>NT$ 790.00</span> ... 590.00 ...");
        let v = scale.verdict(Some(&json!(790)), None).unwrap();
        assert_eq!((v.price.as_str(), v.cents, v.scale, v.currency), ("790", 790, 0, "TWD"));
        assert!(v.confident());
        let v2 = scale.verdict(Some(&json!(79000)), None).unwrap();
        // 79000 looks like cents against the visible 790 anchor → 790 whole.
        assert_eq!((v2.price.as_str(), v2.cents, v2.confident()), ("790", 790, true));
    }

    /// Integer-price locales render no decimals. The display string drops the
    /// trailing `.00`; KRW is zero-decimal so the value never multiplies.
    #[test]
    fn unit_detect_integer_prices_prefix_and_suffix() {
        let pre = PriceScale::from_html("價格 $790 限時");
        assert_eq!(pre.verdict(Some(&json!(790)), None).unwrap().price, "790");

        let suf = PriceScale::from_html("售價 790元 起");
        assert_eq!(suf.verdict(Some(&json!(790)), None).unwrap().price, "790");

        let krw = PriceScale::from_html("₩79000 세일");
        let v = krw.verdict(Some(&json!(79000)), None).unwrap();
        // KRW is zero-decimal → display is the whole value, integer-formatted.
        assert_eq!((v.price.as_str(), v.currency), ("79000", "KRW"));
    }

    /// Cross-field corroboration needs NO page symbol: a whole-unit peer (790)
    /// pins variant.price=79000 as cents purely by the ×100 ratio. This is the
    /// guarantee that survives an incomplete currency-symbol table.
    #[test]
    fn unit_detect_json_cross_field_no_symbol() {
        let scale = PriceScale::from_html("<p>no currency symbols anywhere</p>");
        assert!(!scale.has_anchors);
        // variant 79000 against whole-unit peer 790 → divided back to 790,
        // confidently. Currency is empty → 2-dp default → "790".
        let v = scale
            .verdict(Some(&json!(79000)), Some(dec("790")))
            .unwrap();
        assert_eq!(v.price, "790");
        assert!(v.confident()); // score 3 from cross-field alone
                                // whole-unit value against same peer → unchanged.
        let w = scale.verdict(Some(&json!(790)), Some(dec("790"))).unwrap();
        assert_eq!((w.price.as_str(), w.confident()), ("790", true));
    }

    /// Regression for the 690.00 → 68999.999… f64 round-trip that bit doni.
    /// `690.00` typed as a JSON number must surface as the string `"690"`,
    /// not a float-cast cents integer. Currency is set by an explicit ISO
    /// code field (the only path `detect_currency` trusts — bare `NT$` is
    /// ambiguous and intentionally leaves currency empty).
    #[test]
    fn zero_decimal_strips_trailing_zeros() {
        let scale = PriceScale::from_html(r#"NT$ 690 {"priceCurrency":"TWD"}"#);
        let v = scale.verdict(Some(&json!(690.00)), None).unwrap();
        assert_eq!(v.currency, "TWD"); // TWD is zero-decimal
        assert_eq!(v.price, "690");
    }

    /// Two-decimal currency keeps the cents when non-zero: USD 4.20 prints
    /// as "4.20", not "4.2". Explicit `"currency":"USD"` JSON field is the
    /// canonical signal (bare "USD" in prose isn't enough — see
    /// `detect_currency`).
    #[test]
    fn two_decimal_keeps_meaningful_cents() {
        let scale = PriceScale::from_html(r#"price 4.20 {"currency":"USD"} shown"#);
        let v = scale.verdict(Some(&json!(4.20)), None).unwrap();
        assert_eq!(v.currency, "USD");
        assert_eq!(v.price, "4.20");
    }

    /// `cents`/`scale` are the minor-unit view of the SAME amount as `price`.
    /// Zero-decimal currency (TWD): scale 0, cents == whole units (690).
    /// Two-decimal currency (USD): scale 2, cents == whole × 100 (420).
    /// Both derive from one Decimal, so `cents` can never disagree with the
    /// displayed `price`.
    #[test]
    fn cents_and_scale_mirror_the_display_amount() {
        let twd = PriceScale::from_html(r#"NT$ 690 {"priceCurrency":"TWD"}"#);
        let v = twd.verdict(Some(&json!(690.00)), None).unwrap();
        assert_eq!((v.price.as_str(), v.cents, v.scale), ("690", 690, 0));

        let usd = PriceScale::from_html(r#"price 4.20 {"currency":"USD"} shown"#);
        let u = usd.verdict(Some(&json!(4.20)), None).unwrap();
        assert_eq!((u.price.as_str(), u.cents, u.scale), ("4.20", 420, 2));

        // A whole-dollar USD amount: price strips to "4", cents is 400.
        let usd2 = PriceScale::from_html(r#"price 4.00 {"currency":"USD"} shown"#);
        let u2 = usd2.verdict(Some(&json!(4.00)), None).unwrap();
        assert_eq!((u2.price.as_str(), u2.cents, u2.scale), ("4", 400, 2));
    }

    /// JSON-cents input (variant ships 79000 while page shows 790) is divided
    /// back to whole units; `cents` then reflects the WHOLE amount in minor
    /// units (TWD scale 0 → 790), not the raw JSON 79000.
    #[test]
    fn cents_reflects_normalized_whole_not_raw_json() {
        let scale = PriceScale::from_html(r#"NT$ 790 {"priceCurrency":"TWD"}"#);
        let v = scale.verdict(Some(&json!(79000)), None).unwrap();
        assert_eq!((v.price.as_str(), v.cents, v.scale), ("790", 790, 0));
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

    /// Bare ambiguous symbols (lone `$`, `元`) must NOT set a currency — `$`
    /// maps to USD/TWD/HKD/SGD/…, `元` to CNY/TWD/JPY. (Note: the `NT`-prefixed
    /// `NT$` IS unambiguous and is handled separately — see `nt_dollar_is_twd`.)
    #[test]
    fn ambiguous_symbol_leaves_currency_empty() {
        let html = "價格 $ 790.00 售價 590.00 元";
        assert_eq!(PriceScale::from_html(html).currency, "");
    }

    /// `NT$` is Taiwan-specific notation → TWD (zero-decimal). A TWD shop that
    /// only renders `NT$` with no ISO code (the common easy.co case) must get
    /// scale 0 so its `price_cents` is the whole value, not 100×. Regression
    /// for the doni carousel where `currency=None` 100×'d every price_cents.
    #[test]
    fn nt_dollar_is_twd() {
        let scale = PriceScale::from_html("<span>NT$ 990</span>");
        assert_eq!(scale.currency, "TWD");
        let v = scale.verdict(Some(&json!(990)), None).unwrap();
        assert_eq!((v.price.as_str(), v.cents, v.scale, v.currency), ("990", 990, 0, "TWD"));
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
        // Currency empty → 2-dp default → "50" (50.00 strips to "50").
        assert_eq!((v.price.as_str(), v.confident()), ("50", false));
    }

    /// Regression: a multibyte UTF-8 char in the 16-byte window after a currency
    /// key used to make `&html[at..at+16]` slice on a non-char boundary → panic.
    /// `from_html` must complete; the explicit ISO code is still read.
    #[test]
    fn detect_currency_multibyte_after_key_no_panic() {
        // CJK runs right up against the currency value so the 16-byte window
        // ends mid-codepoint.
        let html = r#"{"priceCurrency":"TWD"}限時搶購咖啡豆特價"#;
        assert_eq!(PriceScale::from_html(html).currency, "TWD");
        // Window that ends inside a CJK char with no ISO code present.
        let html2 = r#"{"currency":"日本語テキストです"}"#;
        assert_eq!(PriceScale::from_html(html2).currency, "");
    }

    /// Regression: a peer near Decimal::MAX overflowed `peer * ONE_HUNDRED`, and
    /// Decimal's `*` panics on overflow. verdict() must return a low-confidence
    /// result instead of crashing.
    #[test]
    fn verdict_huge_peer_does_not_panic() {
        let scale = PriceScale::from_html("<p>no anchors</p>");
        // ~7.9e28, large enough that ×100 overflows Decimal.
        let huge = Decimal::from_str_exact("79000000000000000000000000000").unwrap();
        let v = scale.verdict(Some(&json!(790)), Some(huge)).unwrap();
        // Cross-field corroboration is skipped (overflow) → no +3 from signal 1.
        assert!(!v.confident());
    }

    /// Regression: a single-decimal token ("5.5") used to keep both digits
    /// ("55") because the cents-strip required exactly 2 trailing digits. The
    /// fractional part (1 OR 2 digits) is now dropped; a 3-digit trailing group
    /// is thousands grouping, so those digits stay.
    #[test]
    fn whole_units_handles_single_and_double_decimals() {
        assert_eq!(whole_units("5.5"), Some(5)); // single decimal → 5, not 55
        assert_eq!(whole_units("4.20"), Some(4)); // two decimals → 4
        assert_eq!(whole_units("19,99"), Some(19)); // European decimal → 19
        assert_eq!(whole_units("1,200"), Some(1200)); // 3-digit group = thousands
        assert_eq!(whole_units("1,990.00"), Some(1990)); // grouping + cents
        assert_eq!(whole_units("79000"), Some(79000)); // no separator → integral
    }

    /// Regression: a lower-case ISO code in a currency field ("usd") was missed
    /// because the scan required uppercase. It is now upper-cased before the
    /// whitelist lookup.
    #[test]
    fn detect_currency_lowercase_iso_code() {
        let html = r#"{"currency":"usd"} price 19.99"#;
        assert_eq!(PriceScale::from_html(html).currency, "USD");
        // A non-code 3-letter lowercase run must NOT fabricate a currency.
        let html2 = r#"{"currency":"the quick"}"#;
        assert_eq!(PriceScale::from_html(html2).currency, "");
    }
}
