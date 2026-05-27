//! Python bindings for e-shop-graph. Exposes the pure-engine surface: build a
//! graph from HTML (in-memory or memory-bounded from disk), and run read-only
//! Cypher against a saved `graph.bin`. Tenancy / path semantics stay the
//! caller's concern, mirroring the Rust CLI.

use esg_core::cypher::{self, Value};
use esg_core::store::{save, LoadedGraph};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::path::PathBuf;

fn err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

/// Build a graph from in-memory HTML strings and serialize it to `out_path`
/// (rkyv `graph.bin`). Returns the byte size written. Suited to small batches
/// where holding all HTML in memory is fine.
#[pyfunction]
fn build_graph(pages: Vec<String>, out_path: &str) -> PyResult<usize> {
    let builder = esg_extract::build_from_pages(pages).map_err(err)?;
    let graph = builder.build();
    write_graph(&graph, out_path)
}

/// Memory-bounded build: scan `html_dir` for `*.html`/`*.htm`, mmap each file
/// one at a time, fold into the graph, and serialize to `out_path`. Peak memory
/// is ~one page + the growing graph — independent of page count. This is the
/// enoract handoff path. Returns (page_count, bytes_written).
#[pyfunction]
fn build_graph_from_dir(html_dir: &str, out_path: &str) -> PyResult<(usize, usize)> {
    let dir = PathBuf::from(html_dir);
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(err)? {
        let path = entry.map_err(err)?.path();
        if path.extension().is_some_and(|e| e == "html" || e == "htm") {
            paths.push(path);
        }
    }
    let builder = esg_extract::build_from_files(&paths).map_err(err)?;
    let graph = builder.build();
    let bytes = write_graph(&graph, out_path)?;
    Ok((paths.len(), bytes))
}

/// Run a read-only Cypher query against a saved `graph.bin` (mmap'd zero-copy).
/// Returns a list of row dicts keyed by the RETURN column names. A NodeRef
/// value becomes a nested dict `{"idx", "kind", "name"}`.
#[pyfunction]
fn query(py: Python<'_>, graph_path: &str, cypher_query: &str) -> PyResult<Py<PyList>> {
    let loaded = LoadedGraph::open(&PathBuf::from(graph_path)).map_err(err)?;
    let result = cypher::query(loaded.graph(), cypher_query).map_err(err)?;

    let rows = PyList::empty(py);
    for row in &result.rows {
        let dict = PyDict::new(py);
        for (col, val) in result.columns.iter().zip(row) {
            dict.set_item(col, value_to_py(py, val)?)?;
        }
        rows.append(dict)?;
    }
    Ok(rows.into())
}

fn write_graph(graph: &esg_core::Graph, out_path: &str) -> PyResult<usize> {
    let path = PathBuf::from(out_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
    }
    save(graph, &path).map_err(err)
}

fn value_to_py(py: Python<'_>, v: &Value) -> PyResult<PyObject> {
    Ok(match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_pyobject(py)?.to_owned().unbind().into(),
        Value::Int(i) => i.into_pyobject(py)?.unbind().into(),
        Value::Float(f) => f.into_pyobject(py)?.unbind().into(),
        Value::Str(s) => s.into_pyobject(py)?.unbind().into(),
        Value::NodeRef { idx, kind, name } => {
            let d = PyDict::new(py);
            d.set_item("idx", idx)?;
            d.set_item("kind", kind)?;
            d.set_item("name", name)?;
            d.into_any().unbind()
        }
    })
}

#[pymodule]
fn esg(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(build_graph, m)?)?;
    m.add_function(wrap_pyfunction!(build_graph_from_dir, m)?)?;
    m.add_function(wrap_pyfunction!(query, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
