//! Cypher subset AST. Structure follows ecp's cypher::ast, narrowed to the
//! product-graph query shapes: single/two-hop MATCH, scalar WHERE, property
//! projection. Variable-length paths, WITH, UNION, aggregation are out of scope
//! for the spike.

use crate::schema::{NodeKind, RelType};

#[derive(Debug, Clone)]
pub struct Query {
    pub pattern: Pattern,
    pub where_: Option<Expr>,
    pub return_: Vec<ReturnItem>,
    pub limit: Option<u64>,
}

/// A linear path: node (rel node)* . `rels[i]` connects `nodes[i]` -> `nodes[i+1]`.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub nodes: Vec<NodePat>,
    pub rels: Vec<RelPat>,
}

#[derive(Debug, Clone)]
pub struct NodePat {
    pub var: Option<String>,
    /// Empty = match any kind.
    pub kinds: Vec<NodeKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
}

#[derive(Debug, Clone)]
pub struct RelPat {
    /// Empty = match any relation type.
    pub types: Vec<RelType>,
    pub dir: Direction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub enum Expr {
    BinOp(Op, Box<Expr>, Box<Expr>),
    /// `var.property` — resolved against a node's `props` JSON or its name/kind.
    Prop(String, String),
    Lit(Literal),
}

/// Aggregate function over a column. `Count` may target the whole row
/// (`count(*)`/`count(p)`); the others need a numeric `var.prop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    Count,
    Min,
    Max,
    Sum,
    Avg,
}

#[derive(Debug, Clone)]
pub struct ReturnItem {
    /// `(var, Some(prop))` = `var.prop`; `(var, None)` = the node itself.
    pub var: String,
    pub prop: Option<String>,
    /// When set, this item is an aggregate (e.g. `count(p)`, `min(v.price)`).
    /// Items WITHOUT an agg are the group-by keys.
    pub agg: Option<Agg>,
}
