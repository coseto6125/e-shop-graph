# e-shop-graph (Python bindings)

Zero-copy e-commerce product knowledge graph. Extracts products from real-world
e-commerce HTML (platform JSON, `__NEXT_DATA__`, microdata, JSON-LD), builds a
zero-copy rkyv graph, and serves single-digit-ms read-only Cypher.

```python
import esg

# Build from in-memory HTML strings -> graph.bin
n_bytes = esg.build_graph(["<html>...</html>", "<html>...</html>"], "graph.bin")

# Memory-bounded build: mmap each *.html in a dir, one at a time
n_pages, n_bytes = esg.build_graph_from_dir("/path/to/html_dir", "graph.bin")

# Run Cypher against a saved graph (zero-copy mmap)
rows = esg.query("graph.bin", "MATCH (p:Product) RETURN p.name LIMIT 5")
for row in rows:
    print(row)  # {"p.name": "..."}
```

`esg` is a pure graph engine: callers supply input and output paths; tenancy
and path semantics are the caller's concern.

Licensed under MIT OR Apache-2.0.
