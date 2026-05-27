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
    // esg is a pure graph engine: it takes an input HTML dir and an output
    // graph.bin PATH, and does the graph work. It does NOT interpret the path
    // — tenancy / org isolation is the CALLER's concern (enoract decides where
    // each org/bot graph lives by passing a different out path). Out path
    // defaults to <dir>/graph.bin for local runs.
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next().context("usage: esg <html_dir> [out_graph_path]")?,
    );
    let bin = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("graph.bin"));

    // Collect HTML file paths (NOT their contents — build_from_files mmaps
    // them one at a time so peak memory stays bounded regardless of page count).
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "html" || e == "htm") {
            paths.push(path);
        }
    }
    println!("found {} pages in {dir:?}", paths.len());

    // ── Stage 1+2: extract + build (memory-bounded, mmap per file) ──────────
    let t_build = Instant::now();
    let builder = esg_extract::build_from_files(&paths)?;
    let graph = builder.build();
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
    println!(
        "build: {build_ms:.2}ms  ({} nodes, {} edges)",
        graph.nodes.len(),
        graph.edges.len()
    );

    // ── Stage 3: save (rkyv + atomic write) ─────────────────────────────────
    // Create the caller-supplied output dir if absent (e.g. a fresh
    // <org>/<bot>/ the caller hasn't materialized yet).
    if let Some(parent) = bin.parent() {
        std::fs::create_dir_all(parent)?;
    }
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

    // ── Stage 5: query — hardcoded CSR walk (Product→Brand) ─────────────────
    let t_q = Instant::now();
    let hits = query_products_with_brand(g);
    println!(
        "query: {:.3}ms  (hardcoded: {hits} product→brand pairs)",
        t_q.elapsed().as_secs_f64() * 1000.0
    );

    // ── Stage 6: Cypher — products and their variants ───────────────────────
    let cy = "MATCH (p:Product)-[:HasVariant]->(v:Variant) RETURN p.name, v.name LIMIT 5";
    let t_cy = Instant::now();
    match esg_core::cypher::query(g, cy) {
        Ok(res) => {
            println!(
                "cypher: {:.3}ms  ({} rows) [{}]",
                t_cy.elapsed().as_secs_f64() * 1000.0,
                res.rows.len(),
                res.columns.join(", ")
            );
            for row in res.rows.iter().take(5) {
                let cells: Vec<String> = row.iter().map(fmt_value).collect();
                println!("    {}", cells.join(" | "));
            }
        }
        Err(e) => println!("cypher ERROR: {e}"),
    }

    // ── Stage 6b: inbound query — verifies reverse CSR (Variant <- Product) ──
    let cy_in = "MATCH (v:Variant)<-[:HasVariant]-(p:Product) RETURN p.name, v.name LIMIT 3";
    match esg_core::cypher::query(g, cy_in) {
        Ok(res) => println!("inbound: {} rows (reverse CSR)", res.rows.len()),
        Err(e) => println!("inbound ERROR: {e}"),
    }

    // ── Stage 7: price filter — verifies normalized price_cents (cents) ──────
    // 80000 cents = 800 whole units. Without normalization, variant.price=99000
    // would never compare correctly against whole-unit thresholds.
    let cy2 = "MATCH (v:Variant) WHERE v.price_cents < 80000 RETURN v.name, v.price_cents";
    match esg_core::cypher::query(g, cy2) {
        Ok(res) => {
            println!("price filter (<800): {} variants", res.rows.len());
            for row in res.rows.iter().take(3) {
                let cells: Vec<String> = row.iter().map(fmt_value).collect();
                println!("    {}", cells.join(" | "));
            }
        }
        Err(e) => println!("price filter ERROR: {e}"),
    }

    Ok(())
}

fn fmt_value(v: &esg_core::cypher::Value) -> String {
    use esg_core::cypher::Value::*;
    match v {
        Str(s) => s.clone(),
        Int(i) => i.to_string(),
        Float(f) => f.to_string(),
        Bool(b) => b.to_string(),
        Null => "null".into(),
        NodeRef { kind, name, .. } => format!("{kind}({name})"),
    }
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
