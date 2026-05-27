//! Read-only Cypher subset for the product graph. Layered lexer -> parser ->
//! executor over the zero-copy `ArchivedGraph`. Structure mirrors ecp's
//! cypher module; the supported grammar is narrowed (see parser.rs).

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
