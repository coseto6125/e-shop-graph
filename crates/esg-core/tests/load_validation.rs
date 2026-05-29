//! Load-time validation: a structurally-valid-but-corrupt graph.bin (one that
//! rkyv::access accepts but whose CSR offsets / edge targets / Str slices break
//! the builder's invariants) must be rejected by `LoadedGraph::open` with an
//! Err — never reach a query and panic on an out-of-bounds index.

use esg_core::graph::{Edge, Graph, InEdge, Node, Str, MAGIC, VERSION};
use esg_core::schema::{NodeKind, RelType};
use esg_core::store::{save, LoadedGraph};

/// A minimal well-formed 2-node graph with a single Product->Product edge,
/// used as the baseline each corruption test mutates one field of.
fn well_formed() -> Graph {
    // pool holds "ab" so every node's id/name/props is the 1-byte slice "a"/"b".
    let s = |off: u32| Str { off, len: 1 };
    Graph {
        magic: MAGIC,
        version: VERSION,
        string_pool: b"ab".to_vec(),
        nodes: vec![
            Node {
                kind: NodeKind::Product,
                id: s(0),
                name: s(0),
                props: s(0),
            },
            Node {
                kind: NodeKind::Product,
                id: s(1),
                name: s(1),
                props: s(1),
            },
        ],
        edges: vec![Edge {
            rel: RelType::Brand,
            dst: 1,
        }],
        out_offsets: vec![0, 1, 1],
        in_edges: vec![InEdge {
            rel: RelType::Brand,
            src: 0,
        }],
        in_offsets: vec![0, 0, 1],
    }
}

/// save `g` to a temp path, then attempt to open it. Returns the open result.
fn roundtrip(g: &Graph, tag: &str) -> anyhow::Result<LoadedGraph> {
    let path = std::env::temp_dir().join(format!("esg_load_validation_{tag}.bin"));
    save(g, &path).expect("save");
    LoadedGraph::open(&path)
}

#[test]
fn well_formed_graph_opens() {
    assert!(roundtrip(&well_formed(), "ok").is_ok());
}

#[test]
fn wrong_magic_rejected() {
    let mut g = well_formed();
    g.magic = *b"XXXX";
    assert!(roundtrip(&g, "magic").is_err());
}

#[test]
fn wrong_version_rejected() {
    let mut g = well_formed();
    g.version = VERSION + 1;
    assert!(roundtrip(&g, "version").is_err());
}

#[test]
fn short_out_offsets_rejected() {
    // out_offsets must be nodes.len()+1 (=3); a length-2 array makes the
    // executor index out_offsets[src+1] out of bounds.
    let mut g = well_formed();
    g.out_offsets = vec![0, 1];
    assert!(roundtrip(&g, "short_offsets").is_err());
}

#[test]
fn non_monotonic_offsets_rejected() {
    let mut g = well_formed();
    g.out_offsets = vec![0, 1, 0]; // decreases
    assert!(roundtrip(&g, "nonmono").is_err());
}

#[test]
fn offsets_last_not_edge_count_rejected() {
    let mut g = well_formed();
    g.out_offsets = vec![0, 0, 0]; // last=0 but edges.len()=1
    assert!(roundtrip(&g, "last_mismatch").is_err());
}

#[test]
fn edge_dst_out_of_range_rejected() {
    let mut g = well_formed();
    g.edges = vec![Edge {
        rel: RelType::Brand,
        dst: 99,
    }];
    assert!(roundtrip(&g, "dst_oob").is_err());
}

#[test]
fn in_edge_src_out_of_range_rejected() {
    let mut g = well_formed();
    g.in_edges = vec![InEdge {
        rel: RelType::Brand,
        src: 99,
    }];
    assert!(roundtrip(&g, "src_oob").is_err());
}

#[test]
fn str_overruns_pool_rejected() {
    // props slice claims 9999 bytes over a 2-byte pool — would panic in arch_str.
    let mut g = well_formed();
    g.nodes[0].props = Str { off: 0, len: 9999 };
    assert!(roundtrip(&g, "str_oob").is_err());
}
