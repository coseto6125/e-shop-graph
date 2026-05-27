//! Executes a parsed query against an `ArchivedGraph` (the mmap'd zero-copy
//! form). Strategy: seed candidates from the first node pattern, walk each
//! relationship along CSR edges to extend bindings, apply WHERE per row, then
//! project RETURN. All reads are straight out of the mapped bytes.

use super::ast::*;
use super::value::{QueryResult, Value};
use crate::graph::ArchivedGraph;
use crate::schema::{ArchivedNodeKind, ArchivedRelType, NodeKind, RelType};

/// One binding row: variable position in the pattern -> matched node index.
type Binding = Vec<u32>;

pub fn execute(graph: &ArchivedGraph, q: &Query) -> Result<QueryResult, String> {
    // ── Seed: all nodes matching the first node pattern ──────────────────────
    let mut bindings: Vec<Binding> = Vec::new();
    let first = &q.pattern.nodes[0];
    for (idx, node) in graph.nodes.iter().enumerate() {
        if node_matches(&node.kind, &first.kinds) {
            bindings.push(vec![idx as u32]);
        }
    }

    // ── Extend along each relationship ───────────────────────────────────────
    for (hop, rel) in q.pattern.rels.iter().enumerate() {
        let target_pat = &q.pattern.nodes[hop + 1];
        let mut next: Vec<Binding> = Vec::new();
        for binding in &bindings {
            let src = binding[hop];
            for (neighbor, _) in neighbors(graph, src, rel) {
                if node_matches(&graph.nodes[neighbor as usize].kind, &target_pat.kinds) {
                    let mut extended = binding.clone();
                    extended.push(neighbor);
                    next.push(extended);
                }
            }
        }
        bindings = next;
    }

    // ── Variable name -> position, for WHERE / RETURN resolution ─────────────
    let var_pos = |name: &str| -> Option<usize> {
        q.pattern
            .nodes
            .iter()
            .position(|np| np.var.as_deref() == Some(name))
    };

    // ── WHERE filter ─────────────────────────────────────────────────────────
    if let Some(pred) = &q.where_ {
        bindings.retain(|b| eval_bool(graph, pred, b, &var_pos));
    }

    // ── LIMIT ────────────────────────────────────────────────────────────────
    if let Some(lim) = q.limit {
        bindings.truncate(lim as usize);
    }

    // ── RETURN projection ────────────────────────────────────────────────────
    let columns = q
        .return_
        .iter()
        .map(|ri| {
            let base = match &ri.prop {
                Some(p) => format!("{}.{}", ri.var, p),
                None => ri.var.clone(),
            };
            match ri.agg {
                Some(a) => format!("{}({base})", agg_name(a)),
                None => base,
            }
        })
        .collect();

    let has_agg = q.return_.iter().any(|ri| ri.agg.is_some());
    let rows = if has_agg {
        aggregate_rows(graph, q, &bindings, &var_pos)?
    } else {
        let mut rows = Vec::with_capacity(bindings.len());
        for b in &bindings {
            let mut row = Vec::with_capacity(q.return_.len());
            for ri in &q.return_ {
                let pos = var_pos(&ri.var)
                    .ok_or_else(|| format!("RETURN references unknown variable {}", ri.var))?;
                row.push(project(graph, b[pos], ri.prop.as_deref()));
            }
            rows.push(row);
        }
        rows
    };

    Ok(QueryResult { columns, rows })
}

fn agg_name(a: Agg) -> &'static str {
    match a {
        Agg::Count => "count",
        Agg::Min => "min",
        Agg::Max => "max",
        Agg::Sum => "sum",
        Agg::Avg => "avg",
    }
}

/// Group bindings by the non-aggregate RETURN items (the group keys), then
/// compute each aggregate per group. With no group keys, all bindings form one
/// group (e.g. `RETURN count(p)`).
fn aggregate_rows(
    graph: &ArchivedGraph,
    q: &Query,
    bindings: &[Binding],
    var_pos: &impl Fn(&str) -> Option<usize>,
) -> Result<Vec<Vec<Value>>, String> {
    use std::collections::BTreeMap;

    let key_items: Vec<&ReturnItem> = q.return_.iter().filter(|ri| ri.agg.is_none()).collect();
    let agg_items: Vec<&ReturnItem> = q.return_.iter().filter(|ri| ri.agg.is_some()).collect();

    // group key (string form) -> (projected key values, collected agg inputs).
    let mut groups: BTreeMap<String, (Vec<Value>, Vec<Vec<f64>>)> = BTreeMap::new();

    for b in bindings {
        // Build the group key + its projected display values.
        let mut key_str = String::new();
        let mut key_vals = Vec::with_capacity(key_items.len());
        for ri in &key_items {
            let pos = var_pos(&ri.var)
                .ok_or_else(|| format!("RETURN references unknown variable {}", ri.var))?;
            let v = project(graph, b[pos], ri.prop.as_deref());
            key_str.push_str(&format!("{v:?}\u{1}"));
            key_vals.push(v);
        }
        let entry = groups
            .entry(key_str)
            .or_insert_with(|| (key_vals, vec![Vec::new(); agg_items.len()]));
        // Collect each aggregate's numeric input for this row.
        for (ai, ri) in agg_items.iter().enumerate() {
            let pos = var_pos(&ri.var)
                .ok_or_else(|| format!("RETURN references unknown variable {}", ri.var))?;
            if ri.agg == Some(Agg::Count) {
                entry.1[ai].push(1.0); // count tallies rows; value irrelevant
            } else if let Some(n) = value_as_f64(&project(graph, b[pos], ri.prop.as_deref())) {
                entry.1[ai].push(n);
            }
        }
    }

    // Emit one row per group: key values, then aggregates in RETURN order.
    let mut rows = Vec::with_capacity(groups.len());
    for (_k, (key_vals, agg_inputs)) in groups {
        let mut key_iter = key_vals.into_iter();
        let mut agg_iter = agg_inputs.into_iter();
        let mut row = Vec::with_capacity(q.return_.len());
        for ri in &q.return_ {
            match ri.agg {
                None => row.push(key_iter.next().unwrap_or(Value::Null)),
                Some(a) => {
                    let inputs = agg_iter.next().unwrap_or_default();
                    row.push(fold_agg(a, &inputs));
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn fold_agg(a: Agg, xs: &[f64]) -> Value {
    match a {
        Agg::Count => Value::Int(xs.len() as i64),
        _ if xs.is_empty() => Value::Null,
        Agg::Min => Value::Float(xs.iter().cloned().fold(f64::INFINITY, f64::min)),
        Agg::Max => Value::Float(xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
        Agg::Sum => Value::Float(xs.iter().sum()),
        Agg::Avg => Value::Float(xs.iter().sum::<f64>() / xs.len() as f64),
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// Neighbors of `src` along `rel`. Out-direction uses the CSR out-edges; in
/// uses a scan (the spike graph has no reverse CSR yet — see README backlog).
fn neighbors<'a>(
    graph: &'a ArchivedGraph,
    src: u32,
    rel: &RelPat,
) -> Vec<(u32, &'a ArchivedRelType)> {
    let mut out = Vec::new();
    match rel.dir {
        Direction::Out => {
            let lo = graph.out_offsets[src as usize].to_native() as usize;
            let hi = graph.out_offsets[src as usize + 1].to_native() as usize;
            for edge in &graph.edges[lo..hi] {
                if rel_matches(&edge.rel, &rel.types) {
                    out.push((edge.dst.to_native(), &edge.rel));
                }
            }
        }
        Direction::In => {
            // Reverse CSR: O(in-degree), no full scan.
            let lo = graph.in_offsets[src as usize].to_native() as usize;
            let hi = graph.in_offsets[src as usize + 1].to_native() as usize;
            for in_edge in &graph.in_edges[lo..hi] {
                if rel_matches(&in_edge.rel, &rel.types) {
                    out.push((in_edge.src.to_native(), &in_edge.rel));
                }
            }
        }
    }
    out
}

fn node_matches(kind: &ArchivedNodeKind, wanted: &[NodeKind]) -> bool {
    wanted.is_empty() || wanted.iter().any(|w| archived_node_eq(kind, *w))
}

fn rel_matches(rel: &ArchivedRelType, wanted: &[RelType]) -> bool {
    wanted.is_empty() || wanted.iter().any(|w| archived_rel_eq(rel, *w))
}

fn archived_node_eq(a: &ArchivedNodeKind, b: NodeKind) -> bool {
    *a == match b {
        NodeKind::Product => ArchivedNodeKind::Product,
        NodeKind::Offer => ArchivedNodeKind::Offer,
        NodeKind::Brand => ArchivedNodeKind::Brand,
        NodeKind::Organization => ArchivedNodeKind::Organization,
        NodeKind::Review => ArchivedNodeKind::Review,
        NodeKind::AggregateRating => ArchivedNodeKind::AggregateRating,
        NodeKind::Category => ArchivedNodeKind::Category,
        NodeKind::Person => ArchivedNodeKind::Person,
        NodeKind::Variant => ArchivedNodeKind::Variant,
    }
}

fn archived_rel_eq(a: &ArchivedRelType, b: RelType) -> bool {
    *a == match b {
        RelType::Offers => ArchivedRelType::Offers,
        RelType::Brand => ArchivedRelType::Brand,
        RelType::Manufacturer => ArchivedRelType::Manufacturer,
        RelType::Review => ArchivedRelType::Review,
        RelType::AggregateRating => ArchivedRelType::AggregateRating,
        RelType::IsVariantOf => ArchivedRelType::IsVariantOf,
        RelType::Category => ArchivedRelType::Category,
        RelType::Author => ArchivedRelType::Author,
        RelType::HasVariant => ArchivedRelType::HasVariant,
    }
}

fn eval_bool(
    graph: &ArchivedGraph,
    expr: &Expr,
    binding: &Binding,
    var_pos: &impl Fn(&str) -> Option<usize>,
) -> bool {
    match expr {
        Expr::BinOp(Op::And, l, r) => {
            eval_bool(graph, l, binding, var_pos) && eval_bool(graph, r, binding, var_pos)
        }
        Expr::BinOp(Op::Or, l, r) => {
            eval_bool(graph, l, binding, var_pos) || eval_bool(graph, r, binding, var_pos)
        }
        Expr::BinOp(op, l, r) => {
            let lv = eval_scalar(graph, l, binding, var_pos);
            let rv = eval_scalar(graph, r, binding, var_pos);
            compare(&lv, &rv, *op)
        }
        // A bare property / literal is truthy when non-null/non-empty.
        other => !matches!(eval_scalar(graph, other, binding, var_pos), Value::Null),
    }
}

fn eval_scalar(
    graph: &ArchivedGraph,
    expr: &Expr,
    binding: &Binding,
    var_pos: &impl Fn(&str) -> Option<usize>,
) -> Value {
    match expr {
        Expr::Lit(Literal::Int(i)) => Value::Int(*i),
        Expr::Lit(Literal::Float(f)) => Value::Float(*f),
        Expr::Lit(Literal::Str(s)) => Value::Str(s.clone()),
        Expr::Prop(var, prop) => match var_pos(var) {
            Some(pos) => read_prop(graph, binding[pos], prop),
            None => Value::Null,
        },
        Expr::BinOp(..) => Value::Null, // nested boolean in scalar position: unsupported
    }
}

/// Read `prop` off a node: `name`/`kind` are intrinsic; everything else comes
/// from the node's `props` JSON blob.
fn read_prop(graph: &ArchivedGraph, node_idx: u32, prop: &str) -> Value {
    let node = &graph.nodes[node_idx as usize];
    match prop {
        "name" => Value::Str(arch_str(graph, &node.name)),
        "kind" => Value::Str(format!("{:?}", node.kind)),
        _ => {
            let props_json = arch_str(graph, &node.props);
            match serde_json::from_str::<serde_json::Value>(&props_json) {
                Ok(v) => json_to_value(v.get(prop)),
                Err(_) => Value::Null,
            }
        }
    }
}

fn json_to_value(v: Option<&serde_json::Value>) -> Value {
    match v {
        Some(serde_json::Value::String(s)) => {
            // Prices arrive as JSON strings ("299.99"); coerce numerics so
            // `p.price < 200` works without quoting in the query.
            if let Ok(i) = s.parse::<i64>() {
                Value::Int(i)
            } else if let Ok(f) = s.parse::<f64>() {
                Value::Float(f)
            } else {
                Value::Str(s.clone())
            }
        }
        Some(serde_json::Value::Number(n)) if n.is_i64() => Value::Int(n.as_i64().unwrap()),
        Some(serde_json::Value::Number(n)) => Value::Float(n.as_f64().unwrap()),
        Some(serde_json::Value::Bool(b)) => Value::Bool(*b),
        _ => Value::Null,
    }
}

fn compare(l: &Value, r: &Value, op: Op) -> bool {
    use std::cmp::Ordering;
    let ord = match (as_f64(l), as_f64(r)) {
        (Some(a), Some(b)) => a.partial_cmp(&b),
        _ => match (l, r) {
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            _ => None,
        },
    };
    match (op, ord) {
        (Op::Eq, Some(Ordering::Equal)) => true,
        (Op::Ne, Some(o)) => o != Ordering::Equal,
        (Op::Lt, Some(Ordering::Less)) => true,
        (Op::Le, Some(o)) => o != Ordering::Greater,
        (Op::Gt, Some(Ordering::Greater)) => true,
        (Op::Ge, Some(o)) => o != Ordering::Less,
        _ => false,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn project(graph: &ArchivedGraph, node_idx: u32, prop: Option<&str>) -> Value {
    match prop {
        Some(p) => read_prop(graph, node_idx, p),
        None => {
            let node = &graph.nodes[node_idx as usize];
            Value::NodeRef {
                idx: node_idx,
                kind: format!("{:?}", node.kind),
                name: arch_str(graph, &node.name),
            }
        }
    }
}

/// Resolve an archived `Str` slice against the archived string pool.
fn arch_str(graph: &ArchivedGraph, s: &crate::graph::ArchivedStr) -> String {
    let off = s.off.to_native() as usize;
    let len = s.len.to_native() as usize;
    String::from_utf8_lossy(&graph.string_pool[off..off + len]).into_owned()
}
