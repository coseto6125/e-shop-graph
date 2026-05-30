//! Cypher subset AST. Structure follows ecp's cypher::ast, narrowed to the
//! product-graph query shapes: single/two-hop MATCH, scalar WHERE, property
//! projection. Variable-length paths, WITH, UNION, aggregation are out of scope
//! for the spike.

use crate::schema::{NodeKind, RelType};

#[derive(Debug, Clone)]
pub struct Query {
    pub pattern: Pattern,
    pub where_: Option<Expr>,
    pub distinct: bool,
    pub return_: Vec<ReturnItem>,
    pub order_by: Vec<OrderItem>,
    pub skip: Option<u64>,
    pub limit: Option<u64>,
}

/// One ORDER BY term: a returnable expression and sort direction.
#[derive(Debug, Clone)]
pub struct OrderItem {
    /// `var.prop` or a bare node var, mirroring ReturnItem's addressing.
    pub var: String,
    pub prop: Option<String>,
    pub desc: bool,
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
    /// Inline property constraints, e.g. `(p:Product {name:'X', confident:true})`.
    /// Each must equal the node's corresponding prop. Empty = no constraint.
    pub props: Vec<(String, Literal)>,
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
    Bool(bool),
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

/// Substring match flavor for `STARTS WITH` / `CONTAINS` / `ENDS WITH` / `FUZZY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrMatch {
    StartsWith,
    Contains,
    EndsWith,
    /// `FUZZY` — CJK-friendly recall. The needle is sliced into overlapping
    /// 2-grams and the haystack matches if it contains ANY of them. A CJK
    /// compound the user spells differently from the catalogue ("針織衫" vs the
    /// stocked "針織上衣") still hits on the shared morpheme ("針織"), while a
    /// cross-boundary gram ("織衫") matches nothing, so recall rises without
    /// noise. A needle shorter than 2 chars degrades to a plain `Contains`.
    Fuzzy,
}

#[derive(Debug, Clone)]
pub enum Expr {
    BinOp(Op, Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// `var.property` — resolved against a node's `props` JSON or its name/kind.
    Prop(String, String),
    Lit(Literal),
    /// `<expr> STARTS WITH/CONTAINS/ENDS WITH <literal>`.
    StrMatch(StrMatch, Box<Expr>, String),
    /// `<expr> IS NULL` (true) / `IS NOT NULL` (false in the bool = negated).
    IsNull(Box<Expr>, bool),
    /// `<expr> IN [literals]`.
    In(Box<Expr>, Vec<Literal>),
    /// `<expr> =~ '<regex>'` — Neo4j-compatible regex predicate. Compiled at
    /// parse time so matching is allocation-free per row.
    Regex(Box<Expr>, regex::Regex),
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
    /// For `count(*)` both are conventional: var is empty, `count_star` set.
    pub var: String,
    pub prop: Option<String>,
    /// When set, this item is an aggregate (e.g. `count(p)`, `min(v.price)`).
    /// Items WITHOUT an agg are the group-by keys.
    pub agg: Option<Agg>,
    /// `count(*)` — counts rows without addressing a variable.
    pub count_star: bool,
    /// Output column name from `... AS alias`; defaults to the derived name.
    pub alias: Option<String>,
}
