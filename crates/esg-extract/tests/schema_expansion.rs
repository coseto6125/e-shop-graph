//! Schema-expansion coverage: the JSON-LD ingest now wires entities that were
//! declared in schema.rs but never emitted (Review/Person/Organization), hoists
//! Offer/AggregateRating scalars onto queryable props, and expands ItemList
//! collection pages into one Product per item.

use esg_core::cypher::{self, Value};
use esg_core::graph::ArchivedGraph;
use esg_extract::build_from_pages;

fn rows(html: &str, query: &str) -> Vec<Vec<Value>> {
    let graph = build_from_pages(&[html.to_string()]).expect("build").build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).expect("serialize").to_vec();
    let archived = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&bytes).expect("access");
    cypher::query(archived, query).expect("cypher").rows
}

// A richly-typed Product: offer with availability/condition, manufacturer
// distinct from brand, aggregateRating, two reviews with authors, GTIN/MPN.
const RICH: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"Air Zoom",
 "url":"https://nike.example/air-zoom","sku":"AZ-1","gtin13":"0190000000001","mpn":"NK-AZ",
 "brand":{"@type":"Brand","name":"Nike"},
 "manufacturer":{"@type":"Organization","name":"Nike Inc"},
 "offers":{"@type":"Offer","price":"4200","priceCurrency":"TWD",
   "availability":"https://schema.org/InStock","itemCondition":"https://schema.org/NewCondition",
   "seller":{"@type":"Organization","name":"Nike Store"}},
 "aggregateRating":{"@type":"AggregateRating","ratingValue":4.6,"reviewCount":210},
 "review":[
   {"@type":"Review","reviewBody":"Great","datePublished":"2026-01-01",
    "reviewRating":{"@type":"Rating","ratingValue":5},"author":{"@type":"Person","name":"Alice"}},
   {"@type":"Review","reviewBody":"Okay","reviewRating":{"ratingValue":3},
    "author":{"@type":"Person","name":"Bob"}}
 ]}
</script></head><body><p>NT$4,200</p></body></html>"#;

#[test]
fn offer_scalars_inline_on_product() {
    let r = rows(RICH, r#"MATCH (p:Product) WHERE p.availability = "InStock" RETURN p.item_condition"#);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Str("NewCondition".into()));
}

#[test]
fn identity_props_inline_on_product() {
    // gtin13 keeps its leading zero (string identity, per the correctness fix).
    let r = rows(RICH, r#"MATCH (p:Product) RETURN p.gtin13, p.mpn, p.sku"#);
    assert_eq!(r[0][0], Value::Str("0190000000001".into()));
    assert_eq!(r[0][1], Value::Str("NK-AZ".into()));
}

#[test]
fn aggregate_rating_scalars_queryable() {
    let r = rows(RICH, "MATCH (a:AggregateRating) WHERE a.rating_value >= 4 RETURN a.review_count");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Int(210));
    // mirrored onto Product for single-node filtering
    let pr = rows(RICH, "MATCH (p:Product) WHERE p.review_count > 100 RETURN p.rating_value");
    assert_eq!(pr.len(), 1);
}

#[test]
fn manufacturer_organization_distinct_from_brand() {
    let r = rows(RICH, "MATCH (p:Product)-[:Manufacturer]->(o:Organization) RETURN o.name");
    assert_eq!(r, vec![vec![Value::Str("Nike Inc".into())]]);
    let b = rows(RICH, "MATCH (p:Product)-[:Brand]->(b:Brand) RETURN b.name");
    assert_eq!(b, vec![vec![Value::Str("Nike".into())]]);
}

#[test]
fn reviews_and_authors_wired() {
    let r = rows(
        RICH,
        "MATCH (p:Product)-[:Review]->(r:Review)-[:Author]->(a:Person) WHERE r.rating_value >= 4 RETURN a.name",
    );
    assert_eq!(r, vec![vec![Value::Str("Alice".into())]]);
    // both reviews exist as nodes
    let all = rows(RICH, "MATCH (p:Product)-[:Review]->(r:Review) RETURN count(r)");
    assert_eq!(all[0][0], Value::Int(2));
}

// ItemList collection page: two products under itemListElement -> item.
const ITEMLIST: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"ItemList","itemListElement":[
 {"@type":"ListItem","position":1,"item":{"@type":"Product","name":"Prod A","sku":"A",
   "offers":{"@type":"Offer","price":"100","priceCurrency":"USD"}}},
 {"@type":"ListItem","position":2,"item":{"@type":"Product","name":"Prod B","sku":"B",
   "offers":{"@type":"Offer","price":"200","priceCurrency":"USD"}}}
]}
</script></head><body></body></html>"#;

#[test]
fn item_list_expands_to_multiple_products() {
    let r = rows(ITEMLIST, "MATCH (p:Product) RETURN count(p)");
    assert_eq!(r[0][0], Value::Int(2));
    let names = rows(ITEMLIST, "MATCH (p:Product) RETURN p.name ORDER BY p.name");
    assert_eq!(names[0][0], Value::Str("Prod A".into()));
    assert_eq!(names[1][0], Value::Str("Prod B".into()));
}

// Multi-offer Product: an offers ARRAY must yield one Offer node per element.
const MULTI_OFFER: &str = r#"<!doctype html><html><head>
<script type="application/ld+json">
{"@context":"https://schema.org","@type":"Product","name":"Multi","sku":"M",
 "offers":[
   {"@type":"Offer","price":"100","priceCurrency":"USD","seller":{"name":"S1"}},
   {"@type":"Offer","price":"80","priceCurrency":"USD","seller":{"name":"S2"}}
 ]}
</script></head><body></body></html>"#;

#[test]
fn multi_offer_array_yields_one_node_each() {
    let r = rows(MULTI_OFFER, "MATCH (p:Product)-[:Offers]->(o:Offer) RETURN count(o)");
    assert_eq!(r[0][0], Value::Int(2));
}
