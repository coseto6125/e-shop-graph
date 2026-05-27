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
    Parser { toks: tokens, pos: 0 }.query()
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
        let return_ = self.return_items()?;
        let limit = if self.eat_kw("LIMIT") {
            match self.next() {
                Some(Token::Int(n)) => Some(n as u64),
                other => return Err(format!("LIMIT expects integer, got {other:?}")),
            }
        } else {
            None
        };
        Ok(Query { pattern, where_, return_, limit })
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
        self.expect(&Token::RParen)?;
        Ok(NodePat { var, kinds })
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
        if let Some(agg) = agg_from_name(&first) {
            if matches!(self.peek(), Some(Token::LParen)) {
                self.next();
                let (var, prop) = self.agg_arg()?;
                self.expect(&Token::RParen)?;
                return Ok(ReturnItem { var, prop, agg: Some(agg) });
            }
        }
        // Plain `var` or `var.prop` (a group-by key).
        let prop = if matches!(self.peek(), Some(Token::Dot)) {
            self.next();
            Some(self.ident()?)
        } else {
            None
        };
        Ok(ReturnItem { var: first, prop, agg: None })
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
        let mut lhs = self.cmp_expr()?;
        while self.eat_kw("AND") {
            let rhs = self.cmp_expr()?;
            lhs = Expr::BinOp(Op::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn cmp_expr(&mut self) -> Result<Expr, String> {
        let lhs = self.primary()?;
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

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::LParen) => {
                let e = self.expr()?;
                self.expect(&Token::RParen)?;
                Ok(e)
            }
            Some(Token::Ident(var)) => {
                self.expect(&Token::Dot)?;
                let prop = self.ident()?;
                Ok(Expr::Prop(var, prop))
            }
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
