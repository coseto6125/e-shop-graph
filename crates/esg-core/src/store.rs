//! Persistence: rkyv serialize + atomic write, and zero-copy mmap load.
//! Modeled on ecp-core::registry's atomic-write + warm-attach pattern.

use crate::graph::{ArchivedGraph, Graph, MAGIC, VERSION};
use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use std::fs;
use std::path::Path;

/// Serialize `graph` to rkyv bytes and write atomically (temp + rename) so a
/// reader never observes a half-written `graph.bin`.
pub fn save(graph: &Graph, path: &Path) -> Result<usize> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(graph).context("rkyv serialize graph")?;
    let tmp = path.with_extension("bin.tmp");
    fs::write(&tmp, &bytes).context("write tmp graph.bin")?;
    fs::rename(&tmp, path).context("atomic rename graph.bin")?;
    Ok(bytes.len())
}

/// mmap-backed zero-copy handle. The mmap stays alive as long as this struct
/// does; `graph()` returns a reference INTO the mapped bytes — no copy, no
/// deserialize.
pub struct LoadedGraph {
    _mmap: Mmap,
    ptr: *const ArchivedGraph,
}

// The mmap is read-only and the pointer is derived from it; safe to share.
unsafe impl Send for LoadedGraph {}
unsafe impl Sync for LoadedGraph {}

impl LoadedGraph {
    pub fn open(path: &Path) -> Result<Self> {
        let file = fs::File::open(path).with_context(|| format!("open {path:?}"))?;
        // SAFETY: file is opened read-only; we never mutate the mapping.
        let mmap = unsafe { Mmap::map(&file)? };
        // Validate + locate the archived root within the mapped bytes.
        let archived = rkyv::access::<ArchivedGraph, rkyv::rancor::Error>(&mmap[..])
            .context("rkyv access (corrupt graph.bin?)")?;
        // rkyv::access validates archive STRUCTURE (relative pointers, Vec
        // layouts) but treats our cross-field invariants as opaque integers: a
        // crafted/old/foreign graph.bin can pass access yet carry CSR offsets,
        // edge targets, or Str slices that index out of bounds, panicking later
        // in the executor. Reject such a file here — at the one load chokepoint
        // every consumer (executor, graph.rs API, CLI) flows through — so a bad
        // file is a recoverable error, never a downstream crash.
        validate_invariants(archived)?;
        let ptr = archived as *const ArchivedGraph;
        Ok(Self { _mmap: mmap, ptr })
    }

    #[inline]
    pub fn graph(&self) -> &ArchivedGraph {
        // SAFETY: ptr points into _mmap, which outlives every borrow of self.
        unsafe { &*self.ptr }
    }
}

/// Reject a structurally-valid-but-semantically-corrupt archived graph before
/// any query can dereference its offsets. Checks the invariants the builder
/// guarantees but rkyv does not re-verify on load. All arithmetic is in u64 so
/// a 32-bit `off + len` cannot wrap into a falsely-in-bounds value.
fn validate_invariants(g: &ArchivedGraph) -> Result<()> {
    if g.magic != MAGIC {
        bail!("graph.bin magic mismatch: got {:?}, expected {:?}", g.magic, MAGIC);
    }
    if g.version.to_native() != VERSION {
        bail!(
            "graph.bin version mismatch: got {}, expected {} (rebuild the graph)",
            g.version.to_native(),
            VERSION
        );
    }

    let n = g.nodes.len();
    let pool_len = g.string_pool.len() as u64;

    // CSR offset arrays: one boundary per node plus a final total. They must be
    // non-decreasing and end exactly at the edge count, so every
    // `edges[lo..hi]` slice the executor takes is in bounds.
    let check_offsets = |offsets: &rkyv::vec::ArchivedVec<rkyv::rend::u32_le>,
                         edge_count: usize,
                         label: &str|
     -> Result<()> {
        if offsets.len() != n + 1 {
            bail!("{label} length {} != nodes+1 ({})", offsets.len(), n + 1);
        }
        let mut prev = 0u32;
        for (i, o) in offsets.iter().enumerate() {
            let v = o.to_native();
            if v < prev {
                bail!("{label} not monotonic at {i}: {v} < {prev}");
            }
            prev = v;
        }
        if prev as usize != edge_count {
            bail!("{label} last entry {prev} != edge count {edge_count}");
        }
        Ok(())
    };
    check_offsets(&g.out_offsets, g.edges.len(), "out_offsets")?;
    check_offsets(&g.in_offsets, g.in_edges.len(), "in_offsets")?;

    // Edge endpoints index into nodes — a crafted dst/src out of range panics
    // at `graph.nodes[neighbor]` in the executor.
    for e in g.edges.iter() {
        if e.dst.to_native() as usize >= n {
            bail!("edge dst {} out of range (nodes {n})", e.dst.to_native());
        }
    }
    for e in g.in_edges.iter() {
        if e.src.to_native() as usize >= n {
            bail!("in_edge src {} out of range (nodes {n})", e.src.to_native());
        }
    }

    // Every node string (id/name/props) is an (off,len) slice into the pool;
    // an overrunning slice panics in `Graph::str` / `arch_str`.
    for (i, node) in g.nodes.iter().enumerate() {
        for (s, which) in [(&node.id, "id"), (&node.name, "name"), (&node.props, "props")] {
            let end = s.off.to_native() as u64 + s.len.to_native() as u64;
            if end > pool_len {
                bail!("node {i} {which} Str overruns string_pool ({end} > {pool_len})");
            }
        }
    }
    Ok(())
}
