//! Persistence: rkyv serialize + atomic write, and zero-copy mmap load.
//! Modeled on ecp-core::registry's atomic-write + warm-attach pattern.

use crate::graph::{ArchivedGraph, Graph};
use anyhow::{Context, Result};
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
        let ptr = archived as *const ArchivedGraph;
        Ok(Self { _mmap: mmap, ptr })
    }

    #[inline]
    pub fn graph(&self) -> &ArchivedGraph {
        // SAFETY: ptr points into _mmap, which outlives every borrow of self.
        unsafe { &*self.ptr }
    }
}
