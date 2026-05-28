//! Recursive-descent parser for the Cypher subset:
//!
//!   MATCH <pattern> [WHERE <expr>] RETURN <items> [LIMIT n]
//!
//! Pattern:  (var:Kind) -[:Rel]-> (var:Kind) ...   (also <- for inbound)
//! Expr:     OR > AND > comparison > primary (var.prop | literal)

use super::ast::*;
use super::lexer::{tokenize, Token};
use crate::schema::{NodeKind, RelType};

pub fn parse(input: &str) -> Result<Query, String> {
    let tokens = tokenize(input)?;
    Parser {
        toks: tokens,
        pos: 0,
    }
    .query()
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, t: &Token) -> Result<(), String> {
        match self.next() {
            Some(ref got) if got == t => Ok(()),
            other => Err(format!("expected {t:?}, got {other:?}")),
        }
    }

    /// Match a case-insensitive keyword identifier; advance if it matches.
    fn eat_kw(&mut self, kw: &str) -> bool {
        if let Some(Token::Ident(s)) = self.peek() {
            if s.eq_ignore_ascii_case(kw) {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Token::Ident(s)) => Ok(s),
            other => Err(format!("expected identifier, got {other:?}")),
        }
    }

    fn query(&mut self) -> Result<Query, String> {
        if !self.eat_kw("MATCH") {
            return Err("query must start with MATCH".into());
        }
        let pattern = self.pattern()?;
        let where_ = if self.eat_kw("WHERE") {
            Some(self.expr()?)
        } else {
            None
        };
        if !self.eat_kw("RETURN") {
            return Err("expected RETURN".into());
        }
        let distinct = self.eat_kw("DISTINCT");
        let return_ = self.return_items()?;
        // Clause order per Cypher: ORDER BY, then SKIP, then LIMIT.
        let order_by = if self.eat_kw("ORDER") {
            if !self.eat_kw("BY") {
                return Err("expected BY after ORDER".into());
            }
            self.order_items()?
        } else {
            Vec::new()
        };
        let skip = if self.eat_kw("SKIP") {
            Some(self.uint("SKIP")?)
        } else {
            None
        };
        let limit = if self.eat_kw("LIMIT") {
            Some(self.uint("LIMIT")?)
        } else {
            None
        };
        Ok(Query {
            pattern,
            where_,
            distinct,
            return_,
            order_by,
            skip,
            limit,
        })
    }

    fn uint(&mut self, clause: &str) -> Result<u64, String> {
        match self.next() {
            Some(Token::Int(n)) if n >= 0 => Ok(n as u64),
            other => Err(format!(
                "{clause} expects a non-negative integer, got {other:?}"
            )),
        }
    }

    fn order_items(&mut self) -> Result<Vec<OrderItem>, String> {
        let mut items = vec![self.order_item()?];
        while matches!(self.peek(), Some(Token::Comma)) {
            self.next();
            items.push(self.order_item()?);
        }
        Ok(items)
    }

    fn order_item(&mut self) -> Result<OrderItem, String> {
        let var = self.ident()?;
        let prop = if matches!(self.peek(), Some(Token::Dot)) {
            self.next();
            Some(self.ident()?)
        } else {
            None
        };
        // ASC is the default; DESC reverses. Consume an optional direction kw.
        let desc = if self.eat_kw("DESC") {
            true
        } else {
            self.eat_kw("ASC");
            false
        };
        Ok(OrderItem { var, prop, desc })
    }

    fn pattern(&mut self) -> Result<Pattern, String> {
        let mut nodes = vec![self.node_pat()?];
        let mut rels = Vec::new();
        // (rel node)*
        while matches!(self.peek(), Some(Token::Dash) | Some(Token::ArrowLeft)) {
            rels.push(self.rel_pat()?);
            nodes.push(self.node_pat()?);
        }
        Ok(Pattern { nodes, rels })
    }

    /// `(` [var] [`:`Kind] `)`
    fn node_pat(&mut self) -> Result<NodePat, String> {
        self.expect(&Token::LParen)?;
        let mut var = None;
        if let Some(Token::Ident(_)) = self.peek() {
            var = Some(self.ident()?);
        }
        let mut kinds = Vec::new();
        if matches!(self.peek(), Some(Token::Colon)) {
            self.next();
            let label = self.ident()?;
            let kind = NodeKind::from_label(&label)
                .ok_or_else(|| format!("unknown node label :{label}"))?;
            kinds.push(kind);
        }
        // Optional inline property map: `{ name: 'X', confident: true }`.
        let props = if matches!(self.peek(), Some(Token::LBrace)) {
            self.prop_map()?
        } else {
            Vec::new()
        };
        self.expect(&Token::RParen)?;
        Ok(NodePat { var, kinds, props })
    }

    /// `{ key: <literal> (, key: <literal>)* }` — inline equality constraints.
    fn prop_map(&mut self) -> Result<Vec<(String, Literal)>, String> {
        self.expect(&Token::LBrace)?;
        let mut pairs = Vec::new();
        loop {
            let key = self.ident()?;
            self.expect(&Token::Colon)?;
            pairs.push((key, self.literal()?));
            if matches!(self.peek(), Some(Token::Comma)) {
                self.next();
                continue;
            }
            break;
        }
        self.expect(&Token::RBrace)?;
        Ok(pairs)
    }

    /// A bare literal value (int/float/string/bool) for prop maps and IN lists.
    fn literal(&mut self) -> Result<Literal, String> {
        match self.next() {
            Some(Token::Int(n)) => Ok(Literal::Int(n)),
            Some(Token::Float(f)) => Ok(Literal::Float(f)),
            Some(Token::Str(s)) => Ok(Literal::Str(s)),
            Some(Token::Dash) => match self.next() {
                // Negative numeric literal: `-5`, `-3.2`.
                Some(Token::Int(n)) => Ok(Literal::Int(-n)),
                Some(Token::Float(f)) => Ok(Literal::Float(-f)),
                other => Err(format!("expected number after '-', got {other:?}")),
            },
            Some(Token::Ident(s)) => match s.as_str() {
                "true" | "True" | "TRUE" => Ok(Literal::Bool(true)),
                "false" | "False" | "FALSE" => Ok(Literal::Bool(false)),
                _ => Err(format!("expected a literal value, got identifier {s:?}")),
            },
            other => Err(format!("expected a literal value, got {other:?}")),
        }
    }

    /// Inbound `<-[:Rel]-` or outbound `-[:Rel]->`. The bracket part is optional.
    fn rel_pat(&mut self) -> Result<RelPat, String> {
        let dir = match self.next() {
            Some(Token::Dash) => Direction::Out,
            Some(Token::ArrowLeft) => Direction::In,
            other => return Err(format!("expected relationship start, got {other:?}")),
        };
        let mut types = Vec::new();
        if matches!(self.peek(), Some(Token::LBracket)) {
            self.next();
            if matches!(self.peek(), Some(Token::Colon)) {
                self.next();
                loop {
                    let label = self.ident()?;
                    let rel = RelType::from_label(&label)
                        .ok_or_else(|| format!("unknown rel type :{label}"))?;
                    types.push(rel);
                    if matches!(self.peek(), Some(Token::Pipe)) {
                        self.next();
                        continue;
                    }
                    break;
                }
            }
            self.expect(&Token::RBracket)?;
        }
        // closing arrow / dash
        match self.next() {
            Some(Token::ArrowRight) if dir == Direction::Out => {}
            Some(Token::Dash) if dir == Direction::In => {}
            other => return Err(format!("malformed relationship terminator: {other:?}")),
        }
        Ok(RelPat { types, dir })
    }

    fn return_items(&mut self) -> Result<Vec<ReturnItem>, String> {
        let mut items = vec![self.return_item()?];
        while matches!(self.peek(), Some(Token::Comma)) {
            self.next();
            items.push(self.return_item()?);
        }
        Ok(items)
    }

    fn return_item(&mut self) -> Result<ReturnItem, String> {
        // An identifier followed by `(` is an aggregate: count(p), min(v.price).
        let first = self.ident()?;
        let mut item = if let Some(agg) = agg_from_name(&first) {
            if matches!(self.peek(), Some(Token::LParen)) {
                self.next();
                // `count(*)` — whole-row count with no variable.
                if agg == Agg::Count && matches!(self.peek(), Some(Token::Star)) {
                    self.next();
                    self.expect(&Token::RParen)?;
                    ReturnItem {
                        var: String::new(),
                        prop: None,
                        agg: Some(Agg::Count),
                        count_star: true,
                        alias: None,
                    }
                } else {
                    let (var, prop) = self.agg_arg()?;
                    self.expect(&Token::RParen)?;
                    ReturnItem {
                        var,
                        prop,
                        agg: Some(agg),
                        count_star: false,
                        alias: None,
                    }
                }
            } else {
                // `count` used as a bare group-by key (no parens) — treat as var.
                self.plain_return(first)?
            }
        } else {
            self.plain_return(first)?
        };
        // Optional `AS alias` renames the output column.
        if self.eat_kw("AS") {
            item.alias = Some(self.ident()?);
        }
        Ok(item)
    }

    /// Plain `var` or `var.prop` (a group-by key / projection).
    fn plain_return(&mut self, var: String) -> Result<ReturnItem, String> {
        let prop = if matches!(self.peek(), Some(Token::Dot)) {
            self.next();
            Some(self.ident()?)
        } else {
            None
        };
        Ok(ReturnItem {
            var,
            prop,
            agg: None,
            count_star: false,
            alias: None,
        })
    }

    /// Argument of an aggregate: `var` | `var.prop`. Bare `var` (e.g.
    /// `count(p)`) yields prop=None — a whole-node count.
    fn agg_arg(&mut self) -> Result<(String, Option<String>), String> {
        let var = self.ident()?;
        let prop = if matches!(self.peek(), Some(Token::Dot)) {
            self.next();
            Some(self.ident()?)
        } else {
            None
        };
        Ok((var, prop))
    }

    // ── Expression: OR / AND / comparison / primary ──────────────────────────
    fn expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.and_expr()?;
        while self.eat_kw("OR") {
            let rhs = self.and_expr()?;
            lhs = Expr::BinOp(Op::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.not_expr()?;
        while self.eat_kw("AND") {
            let rhs = self.not_expr()?;
            lhs = Expr::BinOp(Op::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> Result<Expr, String> {
        if self.eat_kw("NOT") {
            return Ok(Expr::Not(Box::new(self.not_expr()?)));
        }
        self.cmp_expr()
    }

    fn cmp_expr(&mut self) -> Result<Expr, String> {
        let lhs = self.primary()?;
        // Keyword-based postfix predicates take priority over the symbol ops.
        if self.eat_kw("IS") {
            let negate = self.eat_kw("NOT");
            if !self.eat_kw("NULL") {
                return Err("expected NULL after IS [NOT]".into());
            }
            return Ok(Expr::IsNull(Box::new(lhs), !negate));
        }
        if self.eat_kw("STARTS") {
            if !self.eat_kw("WITH") {
                return Err("expected WITH after STARTS".into());
            }
            return Ok(Expr::StrMatch(
                StrMatch::StartsWith,
                Box::new(lhs),
                self.str_operand()?,
            ));
        }
        if self.eat_kw("ENDS") {
            if !self.eat_kw("WITH") {
                return Err("expected WITH after ENDS".into());
            }
            return Ok(Expr::StrMatch(
                StrMatch::EndsWith,
                Box::new(lhs),
                self.str_operand()?,
            ));
        }
        if self.eat_kw("CONTAINS") {
            return Ok(Expr::StrMatch(
                StrMatch::Contains,
                Box::new(lhs),
                self.str_operand()?,
            ));
        }
        if self.eat_kw("IN") {
            return Ok(Expr::In(Box::new(lhs), self.list_literal()?));
        }
        if matches!(self.peek(), Some(Token::RegexMatch)) {
            self.next();
            let pattern = self.str_operand()?;
            let re =
                regex::Regex::new(&pattern).map_err(|e| format!("invalid regex in =~ : {e}"))?;
            return Ok(Expr::Regex(Box::new(lhs), re));
        }
        let op = match self.peek() {
            Some(Token::Eq) => Op::Eq,
            Some(Token::Ne) => Op::Ne,
            Some(Token::Lt) => Op::Lt,
            Some(Token::Le) => Op::Le,
            Some(Token::Gt) => Op::Gt,
            Some(Token::Ge) => Op::Ge,
            _ => return Ok(lhs),
        };
        self.next();
        let rhs = self.primary()?;
        Ok(Expr::BinOp(op, Box::new(lhs), Box::new(rhs)))
    }

    /// The string operand of STARTS WITH / CONTAINS / ENDS WITH.
    fn str_operand(&mut self) -> Result<String, String> {
        match self.literal()? {
            Literal::Str(s) => Ok(s),
            other => Err(format!("expected a string operand, got {other:?}")),
        }
    }

    /// `[ <literal> (, <literal>)* ]` for the IN operator.
    fn list_literal(&mut self) -> Result<Vec<Literal>, String> {
        self.expect(&Token::LBracket)?;
        let mut items = Vec::new();
        if !matches!(self.peek(), Some(Token::RBracket)) {
            loop {
                items.push(self.literal()?);
                if matches!(self.peek(), Some(Token::Comma)) {
                    self.next();
                    continue;
                }
                break;
            }
        }
        self.expect(&Token::RBracket)?;
        Ok(items)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::LParen) => {
                let e = self.expr()?;
                self.expect(&Token::RParen)?;
                Ok(e)
            }
            Some(Token::Ident(var)) => match var.as_str() {
                // Bare bool literals — `true`/`false` carry no `.prop`, so they
                // must be recognized before the property path expects a Dot.
                "true" | "True" | "TRUE" => Ok(Expr::Lit(Literal::Bool(true))),
                "false" | "False" | "FALSE" => Ok(Expr::Lit(Literal::Bool(false))),
                _ => {
                    self.expect(&Token::Dot)?;
                    let prop = self.ident()?;
                    Ok(Expr::Prop(var, prop))
                }
            },
            Some(Token::Int(n)) => Ok(Expr::Lit(Literal::Int(n))),
            Some(Token::Float(f)) => Ok(Expr::Lit(Literal::Float(f))),
            Some(Token::Str(s)) => Ok(Expr::Lit(Literal::Str(s))),
            other => Err(format!("unexpected token in expression: {other:?}")),
        }
    }
}

/// Recognize an aggregate function name (case-insensitive). Returns None for
/// ordinary identifiers so they're parsed as group-by keys.
fn agg_from_name(name: &str) -> Option<Agg> {
    match name.to_ascii_lowercase().as_str() {
        "count" => Some(Agg::Count),
        "min" => Some(Agg::Min),
        "max" => Some(Agg::Max),
        "sum" => Some(Agg::Sum),
        "avg" => Some(Agg::Avg),
        _ => None,
    }
}
