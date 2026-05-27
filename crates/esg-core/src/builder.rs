//! Builds a `Graph` from extracted records: interns strings into one pool,
//! dedups nodes by id, flattens edges into CSR order. Mirrors ecp's
//! `GraphBuilder.build()` — pay the indexing cost once at build time so reads
//! are allocation-free.

use crate::graph::{Edge, Graph, InEdge, Node, Str, MAGIC, VERSION};
use crate::schema::{NodeKind, RelType};
use std::collections::HashMap;

#[derive(Default)]
pub struct GraphBuilder {
    pool: Vec<u8>,
    intern: HashMap<String, Str>,
    nodes: Vec<Node>,
    /// node id -> index, for dedup + edge target resolution.
    id_index: HashMap<String, u32>,
    /// (src_idx, rel, dst_id) collected before targets may exist; resolved in build().
    pending_edges: Vec<(u32, RelType, String)>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn intern(&mut self, s: &str) -> Str {
        if let Some(&existing) = self.intern.get(s) {
            return existing;
        }
        let off = self.pool.len() as u32;
        self.pool.extend_from_slice(s.as_bytes());
        let slice = Str {
            off,
            len: s.len() as u32,
        };
        self.intern.insert(s.to_string(), slice);
        slice
    }

    /// Insert (or return existing index of) a node identified by `id`.
    pub fn upsert_node(&mut self, kind: NodeKind, id: &str, name: &str, props: &str) -> u32 {
        if let Some(&idx) = self.id_index.get(id) {
            return idx;
        }
        let node = Node {
            kind,
            id: self.intern(id),
            name: self.intern(name),
            props: self.intern(props),
        };
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        self.id_index.insert(id.to_string(), idx);
        idx
    }

    /// Record an edge `src -> dst_id` with relation `rel`. `dst_id` is resolved
    /// to its node index in `build()`, so the target need not exist yet.
    pub fn add_edge(&mut self, src_idx: u32, rel: RelType, dst_id: &str) {
        self.pending_edges.push((src_idx, rel, dst_id.to_string()));
    }

    /// Flatten into CSR. Edges with an unresolved target id are dropped (a
    /// dangling reference is not a graph edge).
    pub fn build(self) -> Graph {
        let GraphBuilder {
            pool,
            nodes,
            id_index,
            mut pending_edges,
            ..
        } = self;
        let n = nodes.len();

        // Bucket edges per source (forward) and per target (reverse) in one
        // pass, so both CSRs are built from the same resolved edge set.
        let mut per_src: Vec<Vec<Edge>> = vec![Vec::new(); n];
        let mut per_dst: Vec<Vec<InEdge>> = vec![Vec::new(); n];
        for (src, rel, dst_id) in pending_edges.drain(..) {
            if let Some(&dst) = id_index.get(&dst_id) {
                per_src[src as usize].push(Edge { rel, dst });
                per_dst[dst as usize].push(InEdge { rel, src });
            }
        }

        let mut edges = Vec::new();
        let mut out_offsets = Vec::with_capacity(n + 1);
        out_offsets.push(0u32);
        for bucket in per_src {
            edges.extend(bucket);
            out_offsets.push(edges.len() as u32);
        }

        let mut in_edges = Vec::new();
        let mut in_offsets = Vec::with_capacity(n + 1);
        in_offsets.push(0u32);
        for bucket in per_dst {
            in_edges.extend(bucket);
            in_offsets.push(in_edges.len() as u32);
        }

        Graph {
            magic: MAGIC,
            version: VERSION,
            string_pool: pool,
            nodes,
            edges,
            out_offsets,
            in_edges,
            in_offsets,
        }
    }
}
