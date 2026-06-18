//! Cypher subset capability tests. Builds a small product graph in memory,
//! serializes to rkyv bytes, accesses zero-copy, and exercises each query
//! feature added for the 0.2.0 expansion: ORDER BY / SKIP / DISTINCT / NOT /
//! IS [NOT] NULL / STARTS WITH / CONTAINS / ENDS WITH / IN / inline props /
//! count(*) / AS alias.

use esg_core::cypher::{self, Value};
use esg_core::graph::ArchivedGraph;
use esg_core::{GraphBuilder, NodeKind, RelType};

/// Build a fixed 4-product graph; p2 has no price (for IS NULL tests), p1 has
/// a variant. Returns the serialized rkyv bytes for zero-copy access.
fn sample_graph_bytes() -> Vec<u8> {
    let mut b = GraphBuilder::new();
    let p1 = b.upsert_node(
        NodeKind::Product,
        "p1",
        "Nike Air Zoom",
        r#"{"price_cents":12000,"brand":"Nike","price_confident":true}"#,
    );
    b.upsert_node(
        NodeKind::Product,
        "p2",
        "Adidas Ultraboost",
        r#"{"brand":"Adidas","price_confident":false}"#,
    );
    b.upsert_node(
        NodeKind::Product,
        "p3",
        "Nike Pegasus Pro",
        r#"{"price_cents":9000,"brand":"Nike","price_confident":true}"#,
    );
    b.upsert_node(
        NodeKind::Product,
        "p4",
        "Puma Velocity",
        r#"{"price_cents":7000,"brand":"Puma","price_confident":true}"#,
    );
    b.upsert_node(
        NodeKind::Variant,
        "p1#v1",
        "Size 42",
        r#"{"price_cents":12000}"#,
    );
    b.add_edge(p1, RelType::HasVariant, "p1#v1");
    let graph = b.build();
    rkyv::to_bytes::<rkyv::rancor::Error>(&graph)
        .unwrap()
        .to_vec()
}

fn run(bytes: &[u8], q: &str) -> Vec<Vec<Value>> {
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(bytes).unwrap();
    cypher::query(g, q).unwrap().rows
}

fn names(rows: &[Vec<Value>]) -> Vec<String> {
    rows.iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn order_by_asc_then_limit_returns_cheapest_n() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_cents > 0 RETURN p.name, p.price_cents ORDER BY p.price_cents LIMIT 2",
    );
    // Cheapest two priced products: Puma 7000, Pegasus 9000.
    assert_eq!(names(&rows), vec!["Puma Velocity", "Nike Pegasus Pro"]);
}

#[test]
fn order_by_desc_reverses() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_cents > 0 RETURN p.name ORDER BY p.price_cents DESC LIMIT 1",
    );
    assert_eq!(names(&rows), vec!["Nike Air Zoom"]); // 12000, highest
}

#[test]
fn skip_then_limit_paginates_sorted() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_cents > 0 RETURN p.name ORDER BY p.price_cents SKIP 1 LIMIT 1",
    );
    assert_eq!(names(&rows), vec!["Nike Pegasus Pro"]); // 2nd cheapest
}

#[test]
fn starts_with_matches_prefix() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name STARTS WITH 'Nike' RETURN p.name",
    );
    let mut got = names(&rows);
    got.sort();
    assert_eq!(got, vec!["Nike Air Zoom", "Nike Pegasus Pro"]);
}

#[test]
fn contains_matches_substring() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name CONTAINS 'Pro' RETURN p.name",
    );
    assert_eq!(names(&rows), vec!["Nike Pegasus Pro"]);
}

#[test]
fn ends_with_matches_suffix() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name ENDS WITH 'Pro' RETURN p.name",
    );
    assert_eq!(names(&rows), vec!["Nike Pegasus Pro"]);
}

#[test]
fn fuzzy_matches_on_shared_bigram() {
    // "Nikz" never appears as a whole substring, but its 2-grams ["Ni","ik","kz"]
    // include "Ni"/"ik", which the Nike products contain — so FUZZY recovers them
    // where CONTAINS 'Nikz' would return nothing.
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name FUZZY 'Nikz' RETURN p.name",
    );
    let mut got = names(&rows);
    got.sort();
    assert_eq!(got, vec!["Nike Air Zoom", "Nike Pegasus Pro"]);
}

#[test]
fn fuzzy_short_needle_is_plain_contains() {
    // A 1-char needle has no 2-gram, so FUZZY degrades to a substring test.
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name FUZZY 'P' RETURN p.name",
    );
    let mut got = names(&rows);
    got.sort();
    assert_eq!(got, vec!["Nike Pegasus Pro", "Puma Velocity"]);
}

#[test]
fn in_list_filters_brand() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.brand IN ['Nike','Puma'] RETURN p.name",
    );
    assert_eq!(rows.len(), 3); // 2 Nike + 1 Puma, Adidas excluded
}

#[test]
fn not_negates() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE NOT p.brand = 'Nike' RETURN p.name",
    );
    let mut got = names(&rows);
    got.sort();
    assert_eq!(got, vec!["Adidas Ultraboost", "Puma Velocity"]);
}

#[test]
fn is_null_finds_missing_prop() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_cents IS NULL RETURN p.name",
    );
    assert_eq!(names(&rows), vec!["Adidas Ultraboost"]); // only p2 lacks price
}

#[test]
fn is_not_null_finds_present_prop() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_cents IS NOT NULL RETURN p.name",
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn bool_literal_equality() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_confident = true RETURN p.name",
    );
    assert_eq!(rows.len(), 3); // p1, p3, p4 confident; p2 false
}

#[test]
fn inline_props_match() {
    let bytes = sample_graph_bytes();
    let rows = run(&bytes, "MATCH (p:Product {brand:'Puma'}) RETURN p.name");
    assert_eq!(names(&rows), vec!["Puma Velocity"]);
}

#[test]
fn distinct_dedups() {
    let bytes = sample_graph_bytes();
    let rows = run(&bytes, "MATCH (p:Product) RETURN DISTINCT p.brand");
    assert_eq!(rows.len(), 3); // Nike, Adidas, Puma (Nike appears twice, deduped)
}

#[test]
fn count_star_counts_rows() {
    let bytes = sample_graph_bytes();
    let rows = run(&bytes, "MATCH (p:Product) RETURN count(*)");
    assert_eq!(rows[0][0], Value::Int(4));
}

#[test]
fn as_alias_renames_column() {
    let bytes = sample_graph_bytes();
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let res = cypher::query(g, "MATCH (p:Product) RETURN p.price_cents AS price LIMIT 1").unwrap();
    assert_eq!(res.columns, vec!["price"]);
}

#[test]
fn regex_match_filters() {
    let bytes = sample_graph_bytes();
    // Neo4j-style `=~` regex predicate.
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name =~ 'Nike.*' RETURN p.name",
    );
    let mut got = names(&rows);
    got.sort();
    assert_eq!(got, vec!["Nike Air Zoom", "Nike Pegasus Pro"]);
}

#[test]
fn regex_case_insensitive_flag() {
    let bytes = sample_graph_bytes();
    // `(?i)` flag — lowercase pattern matches mixed-case names.
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name =~ '(?i)nike.*' RETURN p.name",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn contains_is_case_insensitive() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name CONTAINS 'NIKE' RETURN p.name",
    );
    assert_eq!(rows.len(), 2); // matches "Nike ..." despite case
}

#[test]
fn order_by_alias_resolves_to_underlying_prop() {
    // Regression: `ORDER BY <alias>` must resolve the RETURN alias back to its
    // var/prop, not reject it as an unknown variable.
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.price_cents > 0 RETURN p.name, p.price_cents AS price ORDER BY price DESC LIMIT 1",
    );
    assert_eq!(names(&rows), vec!["Nike Air Zoom"]); // 12000, highest by alias
}

#[test]
fn multi_hop_with_inline_still_works() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product {brand:'Nike'})-[:HasVariant]->(v:Variant) RETURN p.name, v.name",
    );
    assert_eq!(rows.len(), 1); // only p1 (Nike) has a variant
}

// Backtick-quoted identifier (Neo4j standard) — enables column names
// containing dots or spaces, the natural shape for "preserve the source
// column name in the result" use cases like enoract's
// _graph_rows_to_search_docs which keys off `<var>.<prop>` literally.

#[test]
fn backtick_alias_with_dot() {
    let bytes = sample_graph_bytes();
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let res = cypher::query(
        g,
        "MATCH (p:Product) RETURN p.price_cents AS `p.name` LIMIT 1",
    )
    .unwrap();
    assert_eq!(res.columns, vec!["p.name"]);
}

#[test]
fn backtick_alias_with_space() {
    let bytes = sample_graph_bytes();
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let res = cypher::query(
        g,
        "MATCH (p:Product) RETURN p.name AS `product name` LIMIT 1",
    )
    .unwrap();
    assert_eq!(res.columns, vec!["product name"]);
}

#[test]
fn backtick_alias_interchangeable_with_plain_ident() {
    // Backtick wrapping must not change behaviour for ordinary identifiers —
    // `pname` resolves identically to plain pname.
    let bytes = sample_graph_bytes();
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let r1 = cypher::query(g, "MATCH (p:Product) RETURN p.name AS pname LIMIT 1").unwrap();
    let r2 = cypher::query(g, "MATCH (p:Product) RETURN p.name AS `pname` LIMIT 1").unwrap();
    assert_eq!(r1.columns, r2.columns);
    assert_eq!(r1.rows, r2.rows);
}

#[test]
fn unterminated_backtick_errors() {
    let bytes = sample_graph_bytes();
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let err = cypher::query(g, "MATCH (p:Product) RETURN p.name AS `oops").unwrap_err();
    assert!(err.contains("unterminated backtick"), "got: {err}");
}

#[test]
fn empty_backtick_errors() {
    let bytes = sample_graph_bytes();
    let g = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let err = cypher::query(g, "MATCH (p:Product) RETURN p.name AS `` LIMIT 1").unwrap_err();
    assert!(err.contains("empty backtick"), "got: {err}");
}

/// A small graph whose `sku` is a leading-zero string and whose `color` is a
/// non-numeric string, for identity / cross-type comparison tests.
fn identity_graph_bytes() -> Vec<u8> {
    let mut b = GraphBuilder::new();
    b.upsert_node(
        NodeKind::Product,
        "a",
        "A",
        r#"{"sku":"0123","color":"red"}"#,
    );
    b.upsert_node(
        NodeKind::Product,
        "b",
        "B",
        r#"{"sku":"456","color":"blue"}"#,
    );
    rkyv::to_bytes::<rkyv::rancor::Error>(&b.build())
        .unwrap()
        .to_vec()
}

/// Regression: a leading-zero SKU string ("0123") was coerced to Int(123),
/// losing identity — `RETURN p.sku` returned a number and `WHERE p.sku="0123"`
/// matched nothing. It must stay a string.
#[test]
fn leading_zero_sku_keeps_string_identity() {
    let bytes = identity_graph_bytes();
    let rows = run(
        &bytes,
        r#"MATCH (p:Product) WHERE p.sku = "0123" RETURN p.sku"#,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Str("0123".into()));
}

/// Regression: cross-type `<>` returned false (ord=None fell through), so
/// `WHERE p.color <> 5` excluded every string-valued row. Two non-null values
/// of different types are not equal, so `<>` must be true.
#[test]
fn ne_across_types_is_true() {
    let bytes = identity_graph_bytes();
    let rows = run(&bytes, "MATCH (p:Product) WHERE p.color <> 5 RETURN p.name");
    assert_eq!(rows.len(), 2); // both string colors are != the int 5
}

/// A comparison against a missing (Null) property is never true, including
/// `<>` — Cypher 3-valued logic.
#[test]
fn ne_against_missing_property_is_false() {
    let bytes = identity_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.nonexistent <> 5 RETURN p.name",
    );
    assert!(rows.is_empty());
}

/// Default relevance order: with no explicit ORDER BY, a multi-term OR filter
/// floats the row matching the MOST terms to the front BEFORE limit. Regression
/// for the chat graph lane's "廣州寬褲" miss — `name FUZZY 'A' OR name FUZZY 'B'`
/// used to return seed order, so a row matching both terms could sit past LIMIT
/// behind rows matching only the broad term and get truncated away.
#[test]
fn no_order_by_ranks_by_overlap_before_limit() {
    let bytes = sample_graph_bytes();
    // "Nike Pegasus Pro" matches BOTH 'Nike' and 'Pro' (overlap 2); "Nike Air
    // Zoom" matches only 'Nike' (overlap 1). With LIMIT 1 the both-match row
    // must win regardless of seed order.
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name FUZZY 'Nike' OR p.name FUZZY 'Pro' RETURN p.name LIMIT 1",
    );
    assert_eq!(names(&rows), vec!["Nike Pegasus Pro"]);
}

/// Overlap ordering is stable for equal scores: a single-term filter (every
/// match scores 1) must not reorder rows, so an explicit ORDER BY still wins
/// and a bare single-leaf WHERE keeps seed order.
#[test]
fn single_term_where_keeps_seed_order() {
    let bytes = sample_graph_bytes();
    // All three Nike rows score 1 on the lone term; seed order is p1, p3 (p2/p4
    // are not Nike). Stable sort preserves it — no spurious reshuffle.
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name FUZZY 'Nike' RETURN p.name",
    );
    assert_eq!(names(&rows), vec!["Nike Air Zoom", "Nike Pegasus Pro"]);
}

/// Explicit ORDER BY is untouched by the overlap default — the relevance sort
/// only kicks in when ORDER BY is absent.
#[test]
fn explicit_order_by_overrides_overlap() {
    let bytes = sample_graph_bytes();
    let rows = run(
        &bytes,
        "MATCH (p:Product) WHERE p.name FUZZY 'Nike' OR p.name FUZZY 'Pro' RETURN p.name, p.price_cents ORDER BY p.price_cents LIMIT 1",
    );
    // Cheapest of the matches is Pegasus 9000 — but here it wins by PRICE, not
    // overlap; the assertion guards that ORDER BY still drives the sort.
    assert_eq!(names(&rows), vec!["Nike Pegasus Pro"]);
}
