//! Vertical-slice spike: prove the perf envelope before building out the rest.
//!
//!   extract (JSON-LD parse) -> build (CSR graph) -> save (rkyv) -> load (mmap)
//!   -> query (single-digit ms?)
//!
//! Each stage is timed. The goal: build <2s (network excluded), query in
//! single-digit ms. Run:  `esg <dir-of-html-files>`

use anyhow::{Context, Result};
use esg_core::graph::ArchivedGraph;
use esg_core::store::{save, LoadedGraph};
use esg_core::NodeKind;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .context("usage: esg <dir-of-html-files>")?;
    let dir = PathBuf::from(dir);

    // ── Read fixtures (this is the "network IO" we exclude from the budget) ──
    let mut pages = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "html" || e == "htm") {
            pages.push(std::fs::read_to_string(&path)?);
        }
    }
    println!("loaded {} pages from {dir:?}", pages.len());

    // ── Stage 1+2: extract + build ──────────────────────────────────────────
    let t_build = Instant::now();
    let builder = esg_extract::build_from_pages(pages)?;
    let graph = builder.build();
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
    println!(
        "build: {build_ms:.2}ms  ({} nodes, {} edges)",
        graph.nodes.len(),
        graph.edges.len()
    );

    // ── Stage 3: save (rkyv + atomic write) ─────────────────────────────────
    let bin = dir.join("graph.bin");
    let t_save = Instant::now();
    let bytes = save(&graph, &bin)?;
    println!(
        "save:  {:.2}ms  ({bytes} bytes)",
        t_save.elapsed().as_secs_f64() * 1000.0
    );

    // ── Stage 4: load (mmap zero-copy) ──────────────────────────────────────
    let t_load = Instant::now();
    let loaded = LoadedGraph::open(&bin)?;
    let g = loaded.graph();
    println!("load:  {:.3}ms", t_load.elapsed().as_secs_f64() * 1000.0);

    // ── Stage 5: query — "every Product and its brand" ──────────────────────
    let t_q = Instant::now();
    let hits = query_products_with_brand(g);
    println!(
        "query: {:.3}ms  ({hits} product→brand pairs)",
        t_q.elapsed().as_secs_f64() * 1000.0
    );

    Ok(())
}

/// Walks every Product node, follows its Brand out-edge via CSR. O(nodes+edges),
/// allocation-free reads straight out of the mmap.
fn query_products_with_brand(g: &ArchivedGraph) -> usize {
    let mut hits = 0;
    for (i, node) in g.nodes.iter().enumerate() {
        if node.kind != esg_core::schema::ArchivedNodeKind::Product {
            continue;
        }
        let lo = g.out_offsets[i].to_native() as usize;
        let hi = g.out_offsets[i + 1].to_native() as usize;
        for edge in &g.edges[lo..hi] {
            if edge.rel == esg_core::schema::ArchivedRelType::Brand {
                hits += 1;
            }
        }
    }
    let _ = NodeKind::Product; // keep the schema enum in the dep graph
    hits
}
