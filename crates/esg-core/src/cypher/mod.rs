//! Read-only Cypher subset for the product graph. Layered lexer -> parser ->
//! executor over the zero-copy `ArchivedGraph`. Structure mirrors ecp's
//! cypher module.
//!
//! Supported grammar:
//!   MATCH <pattern> [WHERE <expr>] RETURN [DISTINCT] <items>
//!         [ORDER BY <items> [ASC|DESC]] [SKIP n] [LIMIT n]
//!   pattern : (var:Kind {prop:lit, ...}) -[:Rel|Rel]-> (...)   (also <-)
//!   expr    : OR / AND / NOT, parens; comparisons (= != <> < <= > >=);
//!             IS [NOT] NULL; STARTS WITH / CONTAINS / ENDS WITH (case-
//!             insensitive); IN [lits]; =~ regex (Neo4j-compatible, `(?i)`
//!             flag ok); bool/int/float/string literals
//!   return  : var, var.prop, agg(var.prop), count(*), with optional AS alias
//!             (agg: count/min/max/sum/avg)
//! Regex predicates compile at parse time, so matching is allocation-free
//! per row. Read-only: no CREATE/MERGE/SET/DELETE/WITH/UNION.

pub mod ast;
pub mod executor;
pub mod lexer;
pub mod parser;
pub mod value;

pub use ast::Query;
pub use value::{QueryResult, Value};

use crate::graph::ArchivedGraph;

/// Parse + execute in one call against a mmap'd graph.
pub fn query(graph: &ArchivedGraph, input: &str) -> Result<QueryResult, String> {
    let q = parser::parse(input)?;
    executor::execute(graph, &q)
}
