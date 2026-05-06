//! S-expression DSL parser for [`mimisbrunnr_types::Query`].
//!
//! The DSL is a thin wrapper around the [`Query`] enum that's easy to write
//! by hand and easy to round-trip through [`to_sexpr`]. It is intentionally
//! *not* the SQL surface (that lives in `mimisbrunnr-sql`).
//!
//! ## Grammar
//!
//! ```text
//!   query    = atom
//!   atom     = "(" op args ")"                     ; combinator / op
//!            | "(" "tag" name ")"                  ; HasTag
//!            | "(" "isa" name ")"                  ; IsA
//!            | "(" "attr" name op-cmp value ")"    ; HasAttr
//!            | "(" "related" name oid ")"          ; Related
//!   op       = "and" | "or" | "not"
//!   op-cmp   = "=" | "!=" | "<" | "<=" | ">" | ">=" | "prefix" | "contains"
//!   value    = "\"" string "\"" | integer | float | "@" id     ; @id = TagId raw
//!   name     = identifier (resolved via `resolver` to a TagId)
//!   oid      = "obj:" hex ":" decimal              ; node:hex local:decimal
//! ```
//!
//! Examples:
//!
//! ```text
//!   (and (tag electronic) (attr year = 2024) (not (tag discontinued)))
//!   (isa vehicle)
//!   (or (tag rock) (tag jazz))
//! ```
//!
//! [`Query`]: mimisbrunnr_types::Query

use mimisbrunnr_types::{CmpOp, ObjectId, Query, TagId, Value};

use crate::error::QueryError;

/// Hand-written S-expression parser. Construction takes a tag-name
/// resolver; [`QueryParser::parse`] walks the grammar above.
pub struct QueryParser;

impl QueryParser {
    /// Parse `input` into a [`Query`] tree.
    ///
    /// `resolver` translates bare names (e.g. `electronic`) into [`TagId`]s.
    /// Returning `None` from the resolver yields a [`QueryError::UnknownTag`].
    pub fn parse<F>(input: &str, resolver: F) -> Result<Query, QueryError>
    where
        F: Fn(&str) -> Option<TagId>,
    {
        let tokens = tokenize(input)?;
        let mut cursor = Cursor::new(&tokens);
        let q = parse_atom(&mut cursor, &resolver)?;
        if cursor.pos < tokens.len() {
            return Err(QueryError::Parse {
                position: cursor.pos,
                message: format!("trailing token: {:?}", &tokens[cursor.pos]),
            });
        }
        Ok(q)
    }
}

/// Render `query` back to S-expression form. `name_lookup` translates a
/// [`TagId`] into the human-readable name used in the DSL; if a tag has no
/// name in the resolver, falls back to `@<raw>` (e.g. `@42`) which the
/// parser also accepts.
pub fn to_sexpr<F>(query: &Query, name_lookup: F) -> String
where
    F: Fn(TagId) -> String,
{
    let mut out = String::new();
    render(query, &name_lookup, &mut out);
    out
}

fn render<F>(query: &Query, name_lookup: &F, out: &mut String)
where
    F: Fn(TagId) -> String,
{
    match query {
        Query::HasTag(t) => {
            out.push_str("(tag ");
            out.push_str(&name_lookup(*t));
            out.push(')');
        }
        Query::IsA(t) => {
            out.push_str("(isa ");
            out.push_str(&name_lookup(*t));
            out.push(')');
        }
        Query::HasAttr { key, op, value } => {
            out.push_str("(attr ");
            out.push_str(&name_lookup(*key));
            out.push(' ');
            out.push_str(cmp_to_str(*op));
            out.push(' ');
            push_value(value, out);
            out.push(')');
        }
        Query::Related { predicate, target } => {
            out.push_str("(related ");
            out.push_str(&name_lookup(*predicate));
            out.push(' ');
            out.push_str(&format!("obj:{:x}:{}", target.node_id(), target.local_seq()));
            out.push(')');
        }
        Query::And(qs) => {
            out.push_str("(and");
            for q in qs {
                out.push(' ');
                render(q, name_lookup, out);
            }
            out.push(')');
        }
        Query::Or(qs) => {
            out.push_str("(or");
            for q in qs {
                out.push(' ');
                render(q, name_lookup, out);
            }
            out.push(')');
        }
        Query::Not(q) => {
            out.push_str("(not ");
            render(q, name_lookup, out);
            out.push(')');
        }
    }
}

fn cmp_to_str(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        CmpOp::Prefix => "prefix",
        CmpOp::Contains => "contains",
    }
}

fn push_value(value: &Value, out: &mut String) {
    match value {
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&f.to_string()),
        Value::Timestamp(t) => out.push_str(&format!("ts:{t}")),
        Value::Text(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    other => out.push(other),
                }
            }
            out.push('"');
        }
        Value::Blob(b) => out.push_str(&format!("blob[{}]", b.len())),
        Value::Scoped { context, inner } => {
            out.push_str(&format!("scoped(@{},", context.raw()));
            push_value(inner, out);
            out.push(')');
        }
    }
}

// ---------- Tokeniser ----------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LParen,
    RParen,
    /// Bare word — keyword, operator name, identifier, or numeric literal.
    Word(String),
    /// `"..."` quoted string (used for `Value::Text`).
    Str(String),
    /// `@<digits>` raw tag id literal (e.g. `@42`).
    AtId(u32),
    /// `obj:<hex>:<dec>` ObjectId literal.
    ObjId(ObjectId),
}

fn tokenize(input: &str) -> Result<Vec<Token>, QueryError> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'(' {
            out.push(Token::LParen);
            i += 1;
            continue;
        }
        if c == b')' {
            out.push(Token::RParen);
            i += 1;
            continue;
        }
        if c == b'"' {
            // quoted string with backslash escape
            i += 1;
            let mut buf = String::new();
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    let next = bytes[i + 1];
                    buf.push(next as char);
                    i += 2;
                } else {
                    buf.push(bytes[i] as char);
                    i += 1;
                }
            }
            if i >= bytes.len() {
                return Err(QueryError::Parse {
                    position: i,
                    message: "unterminated string literal".into(),
                });
            }
            i += 1; // closing "
            out.push(Token::Str(buf));
            continue;
        }
        if c == b'@' {
            // raw TagId: @<digits>
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j == start {
                return Err(QueryError::Parse {
                    position: i,
                    message: "expected digits after `@`".into(),
                });
            }
            let raw: u32 = std::str::from_utf8(&bytes[start..j])
                .map_err(|_| QueryError::Parse {
                    position: i,
                    message: "non-utf8 in @id literal".into(),
                })?
                .parse()
                .map_err(|_| QueryError::Parse {
                    position: i,
                    message: "@id literal does not fit u32".into(),
                })?;
            out.push(Token::AtId(raw));
            i = j;
            continue;
        }
        // bare word: anything until whitespace or paren
        let start = i;
        while i < bytes.len() && bytes[i] != b'(' && bytes[i] != b')'
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'"'
        {
            i += 1;
        }
        if i == start {
            return Err(QueryError::Parse {
                position: i,
                message: format!("unexpected character `{}`", c as char),
            });
        }
        let word = std::str::from_utf8(&bytes[start..i])
            .map_err(|_| QueryError::Parse {
                position: start,
                message: "non-utf8 word".into(),
            })?
            .to_string();
        // detect `obj:<hex>:<dec>`
        if let Some(stripped) = word.strip_prefix("obj:") {
            let mut parts = stripped.splitn(2, ':');
            let node_part = parts.next().unwrap_or("");
            let local_part = parts.next().unwrap_or("");
            let node = u16::from_str_radix(node_part, 16).map_err(|_| QueryError::Parse {
                position: start,
                message: format!("bad ObjectId node `{node_part}`"),
            })?;
            let local: u64 = local_part.parse().map_err(|_| QueryError::Parse {
                position: start,
                message: format!("bad ObjectId local `{local_part}`"),
            })?;
            out.push(Token::ObjId(ObjectId::from_parts(node, local)));
            continue;
        }
        out.push(Token::Word(word));
    }
    Ok(out)
}

// ---------- Recursive-descent parser ----------

struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }
    fn bump(&mut self) -> Option<&'a Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }
    fn expect(&mut self, want: &Token) -> Result<(), QueryError> {
        match self.bump() {
            Some(tok) if tok == want => Ok(()),
            Some(tok) => Err(QueryError::Parse {
                position: self.pos.saturating_sub(1),
                message: format!("expected {want:?}, got {tok:?}"),
            }),
            None => Err(QueryError::Parse {
                position: self.pos,
                message: format!("expected {want:?}, got EOF"),
            }),
        }
    }
}

fn parse_atom<F>(cursor: &mut Cursor<'_>, resolver: &F) -> Result<Query, QueryError>
where
    F: Fn(&str) -> Option<TagId>,
{
    cursor.expect(&Token::LParen)?;
    let head = match cursor.bump() {
        Some(Token::Word(w)) => w.clone(),
        Some(other) => {
            return Err(QueryError::Parse {
                position: cursor.pos.saturating_sub(1),
                message: format!("expected operator name, got {other:?}"),
            });
        }
        None => {
            return Err(QueryError::Parse {
                position: cursor.pos,
                message: "expected operator after `(`".into(),
            });
        }
    };

    let result = match head.as_str() {
        "tag" => {
            let tag = parse_name_as_tag(cursor, resolver)?;
            Query::HasTag(tag)
        }
        "isa" => {
            let tag = parse_name_as_tag(cursor, resolver)?;
            Query::IsA(tag)
        }
        "attr" => {
            let key = parse_name_as_tag(cursor, resolver)?;
            let op_word = expect_word(cursor)?;
            let op = match op_word.as_str() {
                "=" => CmpOp::Eq,
                "!=" => CmpOp::Ne,
                "<" => CmpOp::Lt,
                "<=" => CmpOp::Le,
                ">" => CmpOp::Gt,
                ">=" => CmpOp::Ge,
                "prefix" => CmpOp::Prefix,
                "contains" => CmpOp::Contains,
                other => {
                    return Err(QueryError::UnknownOp {
                        op: other.to_string(),
                    });
                }
            };
            let value = parse_value(cursor)?;
            Query::HasAttr { key, op, value }
        }
        "related" => {
            let predicate = parse_name_as_tag(cursor, resolver)?;
            let target = match cursor.bump() {
                Some(Token::ObjId(o)) => *o,
                Some(other) => {
                    return Err(QueryError::Parse {
                        position: cursor.pos.saturating_sub(1),
                        message: format!("expected ObjectId, got {other:?}"),
                    });
                }
                None => {
                    return Err(QueryError::Parse {
                        position: cursor.pos,
                        message: "expected ObjectId after `related <pred>`".into(),
                    });
                }
            };
            Query::Related { predicate, target }
        }
        "and" => {
            let mut children = Vec::new();
            while !matches!(cursor.peek(), Some(Token::RParen) | None) {
                children.push(parse_atom(cursor, resolver)?);
            }
            Query::And(children)
        }
        "or" => {
            let mut children = Vec::new();
            while !matches!(cursor.peek(), Some(Token::RParen) | None) {
                children.push(parse_atom(cursor, resolver)?);
            }
            Query::Or(children)
        }
        "not" => {
            let inner = parse_atom(cursor, resolver)?;
            Query::Not(Box::new(inner))
        }
        other => {
            return Err(QueryError::UnknownOp {
                op: other.to_string(),
            });
        }
    };
    cursor.expect(&Token::RParen)?;
    Ok(result)
}

fn parse_name_as_tag<F>(cursor: &mut Cursor<'_>, resolver: &F) -> Result<TagId, QueryError>
where
    F: Fn(&str) -> Option<TagId>,
{
    match cursor.bump() {
        Some(Token::Word(w)) => resolver(w).ok_or_else(|| QueryError::UnknownTag {
            name: w.clone(),
        }),
        Some(Token::AtId(raw)) => Ok(TagId::new(*raw)),
        Some(other) => Err(QueryError::Parse {
            position: cursor.pos.saturating_sub(1),
            message: format!("expected tag name, got {other:?}"),
        }),
        None => Err(QueryError::Parse {
            position: cursor.pos,
            message: "expected tag name, got EOF".into(),
        }),
    }
}

fn expect_word(cursor: &mut Cursor<'_>) -> Result<String, QueryError> {
    match cursor.bump() {
        Some(Token::Word(w)) => Ok(w.clone()),
        Some(other) => Err(QueryError::Parse {
            position: cursor.pos.saturating_sub(1),
            message: format!("expected operator, got {other:?}"),
        }),
        None => Err(QueryError::Parse {
            position: cursor.pos,
            message: "expected operator, got EOF".into(),
        }),
    }
}

fn parse_value(cursor: &mut Cursor<'_>) -> Result<Value, QueryError> {
    match cursor.bump() {
        Some(Token::Str(s)) => Ok(Value::Text(s.clone())),
        Some(Token::Word(w)) => {
            // Try int first, then float, else fall back to text.
            if let Ok(i) = w.parse::<i64>() {
                Ok(Value::Int(i))
            } else if let Ok(f) = w.parse::<f64>() {
                Ok(Value::Float(f))
            } else {
                // Bare-word value treated as Text. Allows `attr name = bar`.
                Ok(Value::Text(w.clone()))
            }
        }
        Some(other) => Err(QueryError::Parse {
            position: cursor.pos.saturating_sub(1),
            message: format!("expected value, got {other:?}"),
        }),
        None => Err(QueryError::Parse {
            position: cursor.pos,
            message: "expected value, got EOF".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(name: &str) -> Option<TagId> {
        match name {
            "a" => Some(TagId::new(1)),
            "b" => Some(TagId::new(2)),
            "year" => Some(TagId::new(10)),
            "vehicle" => Some(TagId::new(20)),
            _ => None,
        }
    }

    fn name_lookup(t: TagId) -> String {
        match t.raw() {
            1 => "a".into(),
            2 => "b".into(),
            10 => "year".into(),
            20 => "vehicle".into(),
            other => format!("@{other}"),
        }
    }

    #[test]
    fn parse_has_tag() {
        let q = QueryParser::parse("(tag a)", resolver).unwrap();
        assert_eq!(q, Query::HasTag(TagId::new(1)));
    }

    #[test]
    fn parse_and_two_children() {
        let q = QueryParser::parse("(and (tag a) (tag b))", resolver).unwrap();
        assert_eq!(
            q,
            Query::And(vec![
                Query::HasTag(TagId::new(1)),
                Query::HasTag(TagId::new(2))
            ])
        );
    }

    #[test]
    fn parse_round_trip_and() {
        let src = "(and (tag a) (tag b))";
        let q = QueryParser::parse(src, resolver).unwrap();
        let back = to_sexpr(&q, name_lookup);
        assert_eq!(back, src);
    }

    #[test]
    fn parse_attr_eq_int() {
        let q = QueryParser::parse("(attr year = 2024)", resolver).unwrap();
        assert_eq!(
            q,
            Query::HasAttr {
                key: TagId::new(10),
                op: CmpOp::Eq,
                value: Value::Int(2024),
            }
        );
    }

    #[test]
    fn parse_attr_quoted_text() {
        let q = QueryParser::parse(r#"(attr year = "twenty-four")"#, resolver).unwrap();
        assert_eq!(
            q,
            Query::HasAttr {
                key: TagId::new(10),
                op: CmpOp::Eq,
                value: Value::Text("twenty-four".into()),
            }
        );
    }

    #[test]
    fn parse_isa() {
        let q = QueryParser::parse("(isa vehicle)", resolver).unwrap();
        assert_eq!(q, Query::IsA(TagId::new(20)));
    }

    #[test]
    fn parse_complex_query() {
        let src = "(and (tag a) (attr year = 2024) (not (tag b)))";
        let q = QueryParser::parse(src, resolver).unwrap();
        let back = to_sexpr(&q, name_lookup);
        assert_eq!(back, src);
    }

    #[test]
    fn parse_unbalanced_paren() {
        let err = QueryParser::parse("(and (tag a) (tag b)", resolver).unwrap_err();
        assert!(matches!(err, QueryError::Parse { .. }));
    }

    #[test]
    fn parse_unknown_operator() {
        let err = QueryParser::parse("(xor (tag a) (tag b))", resolver).unwrap_err();
        assert!(matches!(err, QueryError::UnknownOp { .. }));
    }

    #[test]
    fn parse_unknown_tag_name() {
        let err = QueryParser::parse("(tag ghost)", resolver).unwrap_err();
        assert!(matches!(err, QueryError::UnknownTag { .. }));
    }

    #[test]
    fn parse_at_id_bypasses_resolver() {
        let q = QueryParser::parse("(tag @42)", resolver).unwrap();
        assert_eq!(q, Query::HasTag(TagId::new(42)));
    }

    #[test]
    fn parse_or_round_trip() {
        let src = "(or (tag a) (tag b))";
        let q = QueryParser::parse(src, resolver).unwrap();
        let back = to_sexpr(&q, name_lookup);
        assert_eq!(back, src);
    }

    #[test]
    fn parse_not_round_trip() {
        let src = "(not (tag a))";
        let q = QueryParser::parse(src, resolver).unwrap();
        let back = to_sexpr(&q, name_lookup);
        assert_eq!(back, src);
    }
}
