//! Query result values. Modeled on ecp's cypher::value but trimmed to what the
//! product-graph subset returns.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A homogeneous-ish list, for array-valued props (e.g. `p.images`). Scalar
    /// elements only — nested objects in the source JSON degrade to `Null`.
    List(Vec<Value>),
    /// A matched node, projected for serialization. `idx` is the graph node id.
    NodeRef {
        idx: u32,
        kind: String,
        name: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}
