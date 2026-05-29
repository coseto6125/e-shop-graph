//! Builds a `Graph` from extracted records: interns strings into one pool,
//! dedups nodes by id, flattens edges into CSR order. Mirrors ecp's
//! `GraphBuilder.build()` — pay the indexing cost once at build time so reads
//! are allocation-free.

use crate::graph::{Edge, Graph, InEdge, Node, Str, MAGIC, VERSION};
use crate::schema::{NodeKind, RelType};
use std::collections::{HashMap, HashSet};

/// Pull the `url` string out of a node's props JSON, if present. Used by
/// `remove_node_by_url` to map a caller-held product URL to its internal node.
fn prop_url(props: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(props)
        .ok()?
        .get("url")?
        .as_str()
        .map(str::to_string)
}

/// Intern `s` into `pool`, deduping via `seen`. Used by `build()` to compact
/// the pool from surviving nodes. Free function (not a method) so it borrows
/// only `pool`/`seen`, sidestepping a self-borrow against the old pool.
fn reintern(s: &str, pool: &mut Vec<u8>, seen: &mut HashMap<String, Str>) -> Str {
    if let Some(&existing) = seen.get(s) {
        return existing;
    }
    let off = pool.len() as u32;
    pool.extend_from_slice(s.as_bytes());
    let slice = Str { off, len: s.len() as u32 };
    seen.insert(s.to_string(), slice);
    slice
}

#[derive(Default)]
pub struct GraphBuilder {
    pool: Vec<u8>,
    intern: HashMap<String, Str>,
    nodes: Vec<Node>,
    /// node id -> index, for dedup + edge target resolution.
    id_index: HashMap<String, u32>,
    /// (src_idx, rel, dst_id) collected before targets may exist; resolved in build().
    pending_edges: Vec<(u32, RelType, String)>,
    /// Node ids marked for removal in `build()` (incremental off-sale). Empty
    /// on a fresh build, so the normal path pays nothing.
    removed: HashSet<String>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rehydrate a builder from an existing (owned) `Graph` — the entry point
    /// for INCREMENTAL rebuilds. Every node and forward edge is re-interned, so
    /// the caller can then `upsert_node` (add/overwrite) or `remove_node`
    /// (off-sale) a handful of items and `build()` a fresh compact graph
    /// WITHOUT re-crawling the whole site. Only forward (out) edges are
    /// replayed; `build()` regenerates the reverse CSR from them.
    ///
    /// Takes an owned `Graph` (obtained by deserializing a loaded `graph.bin`),
    /// not the zero-copy `ArchivedGraph` — incremental rebuild is rare and
    /// mutates anyway, so the one-time deserialize cost is irrelevant, and the
    /// owned `Graph`'s `str()` / `out_edges()` helpers keep this readable.
    pub fn from_graph(graph: &Graph) -> Self {
        let mut b = Self::new();
        // Pass 1: intern every node so edge targets resolve by id in build().
        for node in &graph.nodes {
            b.upsert_node(node.kind, graph.str(&node.id), graph.str(&node.name), graph.str(&node.props));
        }
        // Pass 2: replay forward edges as (src_idx, rel, dst_id). src_idx is the
        // dense index we just assigned (node order is preserved in pass 1), and
        // dst_id is the target node's id string.
        for (src_idx, _node) in graph.nodes.iter().enumerate() {
            for edge in graph.out_edges(src_idx as u32) {
                let dst_id = graph.str(&graph.nodes[edge.dst as usize].id);
                b.add_edge(src_idx as u32, edge.rel, dst_id);
            }
        }
        b
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

    /// Insert a node identified by `id`, or — if `id` already exists — OVERWRITE
    /// its `name`/`props`/`kind` in place and return the existing index. The
    /// overwrite is what makes incremental re-ingest work: a re-crawled product
    /// whose price changed updates the node rather than being dropped as a dup.
    /// (Within a single fresh build the same id is only re-seen as a redundant
    /// re-statement of identical data, so the overwrite is a harmless no-op
    /// there.) New strings are interned; the old `Str` slices are abandoned in
    /// the pool — `build()` compacts the pool, so stale bytes never ship.
    pub fn upsert_node(&mut self, kind: NodeKind, id: &str, name: &str, props: &str) -> u32 {
        let name_str = self.intern(name);
        let props_str = self.intern(props);
        if let Some(&idx) = self.id_index.get(id) {
            let node = &mut self.nodes[idx as usize];
            node.kind = kind;
            node.name = name_str;
            node.props = props_str;
            self.removed.remove(id); // re-stating a node un-removes it
            return idx;
        }
        let node = Node {
            kind,
            id: self.intern(id),
            name: name_str,
            props: props_str,
        };
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        self.id_index.insert(id.to_string(), idx);
        idx
    }

    /// Mark a node (by id) for removal. Takes effect in `build()`: the node and
    /// every edge touching it (in or out) are dropped, and surviving nodes are
    /// renumbered into a dense CSR. A no-op for an id that isn't present.
    /// This is how a product going off-sale leaves the graph during an
    /// incremental rebuild without re-crawling the whole site.
    pub fn remove_node(&mut self, id: &str) {
        if self.id_index.contains_key(id) {
            self.removed.insert(id.to_string());
        }
    }

    /// Mark a node for removal by its `url` prop rather than its internal id.
    /// The internal node id is source-dependent (a platform `handle`, a JSON-LD
    /// `@id`, an sku…) and not something a caller holds; the product URL is.
    /// This scans node props for a matching `"url"` and removes the first hit
    /// (urls are unique per product). Returns true if a node was marked.
    /// O(nodes) — fine for the handful of off-sale ids in an incremental run.
    pub fn remove_node_by_url(&mut self, url: &str) -> bool {
        for node in &self.nodes {
            let props = std::str::from_utf8(
                &self.pool[node.props.off as usize..(node.props.off + node.props.len) as usize],
            )
            .expect("string_pool utf8");
            if prop_url(props).is_some_and(|u| u == url) {
                let id = std::str::from_utf8(
                    &self.pool[node.id.off as usize..(node.id.off + node.id.len) as usize],
                )
                .expect("string_pool utf8")
                .to_string();
                self.removed.insert(id);
                return true;
            }
        }
        false
    }

    /// Record an edge `src -> dst_id` with relation `rel`. `dst_id` is resolved
    /// to its node index in `build()`, so the target need not exist yet.
    pub fn add_edge(&mut self, src_idx: u32, rel: RelType, dst_id: &str) {
        self.pending_edges.push((src_idx, rel, dst_id.to_string()));
    }

    /// Flatten into CSR. Edges with an unresolved target id are dropped (a
    /// dangling reference is not a graph edge).
    ///
    /// Removed nodes (see `remove_node`) are excluded and survivors are
    /// renumbered into a dense index space; the string pool is rebuilt from
    /// the survivors only, so `upsert_node`'s abandoned-string slices and any
    /// removed node's bytes are compacted away. On a fresh build with nothing
    /// removed this still recompacts — cheap relative to extraction, and it
    /// keeps the pool tight without a separate code path.
    pub fn build(self) -> Graph {
        let GraphBuilder {
            pool,
            nodes,
            id_index,
            pending_edges,
            removed,
            intern: _,
        } = self;

        // 1. Old index -> new (dense) index for surviving nodes. Removed nodes
        //    map to nothing; every edge touching one is dropped below.
        let resolve_old = |old: &str| id_index.get(old).copied();
        let mut old_to_new: Vec<Option<u32>> = vec![None; nodes.len()];
        let mut next = 0u32;
        for (old_idx, node) in nodes.iter().enumerate() {
            let id = std::str::from_utf8(&pool[node.id.off as usize..(node.id.off + node.id.len) as usize])
                .expect("string_pool utf8");
            if !removed.contains(id) {
                old_to_new[old_idx] = Some(next);
                next += 1;
            }
        }
        let n = next as usize;

        // 2. Rebuild nodes + a fresh, compact string pool from survivors only.
        //    Re-interning drops abandoned (overwritten) and removed bytes.
        let mut new_pool: Vec<u8> = Vec::with_capacity(pool.len());
        let mut new_intern: HashMap<String, Str> = HashMap::new();
        let str_of = |st: &Str| -> &str {
            std::str::from_utf8(&pool[st.off as usize..(st.off + st.len) as usize]).expect("string_pool utf8")
        };
        let mut new_nodes: Vec<Node> = Vec::with_capacity(n);
        for (old_idx, node) in nodes.iter().enumerate() {
            if old_to_new[old_idx].is_none() {
                continue;
            }
            let id = reintern(str_of(&node.id), &mut new_pool, &mut new_intern);
            let name = reintern(str_of(&node.name), &mut new_pool, &mut new_intern);
            let props = reintern(str_of(&node.props), &mut new_pool, &mut new_intern);
            new_nodes.push(Node { kind: node.kind, id, name, props });
        }

        // 3. Bucket edges per source (forward) and per target (reverse) in one
        //    pass, on the NEW dense indices. An edge whose src or dst was
        //    removed (or whose dst id never resolved) is dropped.
        let mut per_src: Vec<Vec<Edge>> = vec![Vec::new(); n];
        let mut per_dst: Vec<Vec<InEdge>> = vec![Vec::new(); n];
        for (src_old, rel, dst_id) in pending_edges {
            let Some(src) = old_to_new.get(src_old as usize).copied().flatten() else {
                continue;
            };
            let Some(dst) = resolve_old(&dst_id).and_then(|d| old_to_new[d as usize]) else {
                continue;
            };
            per_src[src as usize].push(Edge { rel, dst });
            per_dst[dst as usize].push(InEdge { rel, src });
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
            string_pool: new_pool,
            nodes: new_nodes,
            edges,
            out_offsets,
            in_edges,
            in_offsets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Find a node by id in an owned Graph; returns (idx, props).
    fn node<'g>(g: &'g Graph, id: &str) -> Option<(u32, &'g str)> {
        g.nodes
            .iter()
            .enumerate()
            .find(|(_, nd)| g.str(&nd.id) == id)
            .map(|(i, nd)| (i as u32, g.str(&nd.props)))
    }

    /// upsert on an existing id OVERWRITES name/props — this is what lets an
    /// incremental re-ingest update a changed price instead of dropping the
    /// re-crawled product as a duplicate.
    #[test]
    fn upsert_existing_id_overwrites_props() {
        let mut b = GraphBuilder::new();
        b.upsert_node(NodeKind::Product, "p1", "Tee", r#"{"price":"690"}"#);
        let idx1 = b.upsert_node(NodeKind::Product, "p1", "Tee", r#"{"price":"590"}"#);
        // Same id → same index (no second node), props reflect the latest write.
        let g = b.build();
        assert_eq!(g.nodes.len(), 1);
        let (idx2, props) = node(&g, "p1").unwrap();
        assert_eq!(idx2, idx1);
        assert!(props.contains("590"), "props should be overwritten: {props}");
        assert!(!props.contains("690"), "stale price must be gone: {props}");
    }

    /// remove_node drops the node AND every edge touching it, and renumbers the
    /// survivors into a dense CSR. A product going off-sale leaves cleanly.
    #[test]
    fn remove_node_drops_node_and_its_edges() {
        let mut b = GraphBuilder::new();
        b.upsert_node(NodeKind::Product, "p1", "Tee", "{}");
        b.upsert_node(NodeKind::Brand, "b1", "Acme", "{}");
        b.upsert_node(NodeKind::Product, "p2", "Cap", "{}");
        // p1 -> b1, p2 -> b1
        let p1 = node_idx(&b, "p1");
        let p2 = node_idx(&b, "p2");
        b.add_edge(p1, RelType::Brand, "b1");
        b.add_edge(p2, RelType::Brand, "b1");
        b.remove_node("p1");
        let g = b.build();

        // p1 gone; p2 and b1 survive and are densely renumbered.
        assert_eq!(g.nodes.len(), 2);
        assert!(node(&g, "p1").is_none());
        let (p2_new, _) = node(&g, "p2").unwrap();
        let (b1_new, _) = node(&g, "b1").unwrap();
        // p2 -> b1 edge survived and points at the renumbered brand.
        let p2_out = g.out_edges(p2_new);
        assert_eq!(p2_out.len(), 1);
        assert_eq!(p2_out[0].dst, b1_new);
        // b1's only remaining in-edge is from p2 (p1's was dropped).
        let b1_in = g.in_edges(b1_new);
        assert_eq!(b1_in.len(), 1);
        assert_eq!(b1_in[0].src, p2_new);
    }

    /// from_graph rehydrates an existing graph; a subsequent build() with no
    /// changes reproduces the same nodes + edges (round-trip identity). This is
    /// the foundation of incremental rebuild: load → (mutate) → build.
    #[test]
    fn from_graph_round_trips() {
        let mut b = GraphBuilder::new();
        b.upsert_node(NodeKind::Product, "p1", "Tee", r#"{"price":"690"}"#);
        b.upsert_node(NodeKind::Brand, "b1", "Acme", "{}");
        let p1 = node_idx(&b, "p1");
        b.add_edge(p1, RelType::Brand, "b1");
        let g1 = b.build();

        // Rehydrate and rebuild untouched.
        let g2 = GraphBuilder::from_graph(&g1).build();
        assert_eq!(g2.nodes.len(), g1.nodes.len());
        let (p, props) = node(&g2, "p1").unwrap();
        assert!(props.contains("690"));
        // The Brand edge survived the round-trip.
        let out = g2.out_edges(p);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rel, RelType::Brand);
        assert_eq!(g2.str(&g2.nodes[out[0].dst as usize].id), "b1");
    }

    /// Incremental end-to-end: rehydrate, change one price, drop one product,
    /// add a new one — all in one build, no full re-extraction.
    #[test]
    fn incremental_update_change_remove_add() {
        let mut b = GraphBuilder::new();
        b.upsert_node(NodeKind::Product, "p1", "Tee", r#"{"price":"690"}"#);
        b.upsert_node(NodeKind::Product, "p2", "Cap", r#"{"price":"300"}"#);
        let g0 = b.build();

        let mut b2 = GraphBuilder::from_graph(&g0);
        b2.upsert_node(NodeKind::Product, "p1", "Tee", r#"{"price":"590"}"#); // price change
        b2.remove_node("p2"); // off-sale
        b2.upsert_node(NodeKind::Product, "p3", "Sock", r#"{"price":"120"}"#); // new
        let g = b2.build();

        assert_eq!(g.nodes.len(), 2);
        assert!(node(&g, "p1").unwrap().1.contains("590"));
        assert!(node(&g, "p2").is_none());
        assert!(node(&g, "p3").unwrap().1.contains("120"));
    }

    fn node_idx(b: &GraphBuilder, id: &str) -> u32 {
        *b.id_index.get(id).expect("node present")
    }

    /// remove_node_by_url maps a product URL (what a caller holds) to the
    /// internal node id (a source-specific handle the caller never sees) and
    /// removes it. Regression for the doni case where the node id is the
    /// platform `handle`, NOT the url, so removing by url is the only way an
    /// off-sale signal (which carries a url) can take effect.
    #[test]
    fn remove_node_by_url_matches_props_url() {
        let mut b = GraphBuilder::new();
        // id = handle "h1", but the caller only knows the url.
        b.upsert_node(NodeKind::Product, "h1", "Tee", r#"{"url":"/products/tee","price":"690"}"#);
        b.upsert_node(NodeKind::Product, "h2", "Cap", r#"{"url":"/products/cap","price":"300"}"#);
        assert!(b.remove_node_by_url("/products/tee"));
        assert!(!b.remove_node_by_url("/products/nope")); // no match → false
        let g = b.build();
        assert_eq!(g.nodes.len(), 1);
        assert!(node(&g, "h1").is_none()); // removed by url despite id="h1"
        assert!(node(&g, "h2").is_some());
    }
}
