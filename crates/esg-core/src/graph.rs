//! Zero-copy product graph. Design lifted from ecp's `ZeroCopyGraph`:
//!
//! - **string pool**: every string lives once in `string_pool`; nodes store a
//!   `(u32 offset, u32 len)` slice instead of an owned `String`. No per-node
//!   allocation, no fragmentation.
//! - **CSR adjacency**: out-edges of node `i` are `out_offsets[i]..out_offsets[i+1]`.
//!   Contiguous, cache-friendly, one mmap brings the whole graph live.
//! - **rkyv zero-copy**: `load()` mmaps `graph.bin` and `rkyv::access`-es it
//!   WITHOUT deserializing — the bytes ARE the struct. This is what makes
//!   single-digit-ms queries possible.

use crate::schema::{NodeKind, RelType};
use rkyv::{Archive, Deserialize, Serialize};

/// A `(offset, len)` slice into `Graph::string_pool`. Resolved via `Graph::str`.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy)]
#[rkyv(derive(Debug))]
pub struct Str {
    pub off: u32,
    pub len: u32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct Node {
    pub kind: NodeKind,
    /// Stable identity (schema.org @id, SKU, or synthesized URL#type).
    pub id: Str,
    /// Display name (Product.name, Brand.name, …).
    pub name: Str,
    /// JSON blob of remaining scalar props (price, ratingValue, …). Parsed
    /// lazily by query consumers; kept opaque here to stay schema-agnostic.
    pub props: Str,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy)]
#[rkyv(derive(Debug))]
pub struct Edge {
    pub rel: RelType,
    /// Target node index into `Graph::nodes`.
    pub dst: u32,
}

/// Reverse-adjacency entry: for a target node, "who points at me, via what".
/// Mirror of `Edge` with `src` instead of `dst`, grouped by target in CSR.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy)]
#[rkyv(derive(Debug))]
pub struct InEdge {
    pub rel: RelType,
    /// Source node index that points at this target.
    pub src: u32,
}

#[derive(Archive, Serialize, Deserialize, Debug)]
#[rkyv(derive(Debug))]
pub struct Graph {
    pub magic: [u8; 4],
    pub version: u32,
    pub string_pool: Vec<u8>,
    pub nodes: Vec<Node>,
    /// Flattened out-edges, grouped by source node (CSR).
    pub edges: Vec<Edge>,
    /// CSR boundaries: `edges[out_offsets[i]..out_offsets[i+1]]` are node i's
    /// out-edges. Length is `nodes.len() + 1`.
    pub out_offsets: Vec<u32>,
    /// Reverse CSR: `in_edges[in_offsets[i]..in_offsets[i+1]]` are the edges
    /// pointing AT node i. Answers "who references me" (Cypher inbound `<-`) in
    /// O(in-degree) instead of scanning every edge. Length `nodes.len() + 1`.
    pub in_edges: Vec<InEdge>,
    pub in_offsets: Vec<u32>,
}

pub const MAGIC: [u8; 4] = *b"ESG1";
/// Bumped 2→3 for the schema expansion: Review/Person/Organization/Category
/// nodes and the new BroaderCategory edge are now emitted. rkyv discriminants
/// stayed stable (new variants appended at the end), so the bump is a semantic
/// signal for consumers — a v3 graph with no Review nodes means the page had
/// none, vs a v2 graph where the extractor simply never produced them.
pub const VERSION: u32 = 3;

impl Graph {
    /// Resolve a `Str` slice against the pool. Panics on out-of-bounds — a
    /// corrupt pool is a build-time invariant violation, not a runtime case.
    #[inline]
    pub fn str(&self, s: &Str) -> &str {
        let start = s.off as usize;
        let end = start + s.len as usize;
        std::str::from_utf8(&self.string_pool[start..end]).expect("string_pool utf8")
    }

    /// Out-edges of `node_idx` as a slice — O(1), no allocation.
    #[inline]
    pub fn out_edges(&self, node_idx: u32) -> &[Edge] {
        let i = node_idx as usize;
        let lo = self.out_offsets[i] as usize;
        let hi = self.out_offsets[i + 1] as usize;
        &self.edges[lo..hi]
    }

    /// In-edges (who points at `node_idx`) as a slice — O(1), no allocation.
    #[inline]
    pub fn in_edges(&self, node_idx: u32) -> &[InEdge] {
        let i = node_idx as usize;
        let lo = self.in_offsets[i] as usize;
        let hi = self.in_offsets[i + 1] as usize;
        &self.in_edges[lo..hi]
    }
}
