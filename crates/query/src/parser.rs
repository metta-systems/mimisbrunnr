use {
    mimisbrunnr_ontology::ImplicationDag,
    mimisbrunnr_types::{CmpOp, Query, TagId, Value},
};

use crate::QueryError;

/// Parses query strings into `Query` ASTs.
///
/// Grammar:
/// ```text
///   query     = or_expr
///   or_expr   = and_expr ("OR" and_expr)*
///   and_expr  = not_expr ("AND" not_expr)*
///   not_expr  = "NOT" atom | atom
///   atom      = tag_query | attr_query | "(" query ")"
///   tag_query = identifier
///   attr_query = identifier op value
///   op        = "=" | "!=" | "<" | "<=" | ">" | ">="
///   value     = string | number
/// ```
///
/// Examples:
/// - `electronic AND portable`
/// - `project=vesper AND source AND lang=rust`
/// - `electronic AND year:2024 AND NOT discontinued`
/// - `(rock OR jazz) AND year>2020`
pub struct QueryParser<'a> {
    dag: &'a ImplicationDag,
}

impl<'a> QueryParser<'a> {
    pub fn new(dag: &'a ImplicationDag) -> Self {
        Self { dag }
    }

    /// Parse a query string into a Query AST.
    pub fn parse(&self, input: &str) -> Result<Query, QueryError> {
        let tokens = tokenize(input)?;
        if tokens.is_empty() {
            return Err(QueryError::EmptyQuery);
        }
        let mut pos = 0;
        let result = self.parse_or(&tokens, &mut pos)?;
        if pos < tokens.len() {
            return Err(QueryError::Parse {
                position: pos,
                message: format!("unexpected token: {:?}", tokens[pos]),
            });
        }
        Ok(result)
    }

    fn parse_or(&self, tokens: &[Token], pos: &mut usize) -> Result<Query, QueryError> {
        let mut left = self.parse_and(tokens, pos)?;

        while *pos < tokens.len() && tokens[*pos] == Token::Or {
            *pos += 1;
            let right = self.parse_and(tokens, pos)?;
            left = match left {
                Query::Or(mut parts) => {
                    parts.push(right);
                    Query::Or(parts)
                }
                _ => Query::Or(vec![left, right]),
            };
        }

        Ok(left)
    }

    fn parse_and(&self, tokens: &[Token], pos: &mut usize) -> Result<Query, QueryError> {
        let mut left = self.parse_not(tokens, pos)?;

        while *pos < tokens.len() && tokens[*pos] == Token::And {
            *pos += 1;
            let right = self.parse_not(tokens, pos)?;
            left = match left {
                Query::And(mut parts) => {
                    parts.push(right);
                    Query::And(parts)
                }
                _ => Query::And(vec![left, right]),
            };
        }

        Ok(left)
    }

    fn parse_not(&self, tokens: &[Token], pos: &mut usize) -> Result<Query, QueryError> {
        if *pos < tokens.len() && tokens[*pos] == Token::Not {
            *pos += 1;
            let inner = self.parse_atom(tokens, pos)?;
            return Ok(Query::Not(Box::new(inner)));
        }
        self.parse_atom(tokens, pos)
    }

    fn parse_atom(&self, tokens: &[Token], pos: &mut usize) -> Result<Query, QueryError> {
        if *pos >= tokens.len() {
            return Err(QueryError::Parse {
                position: *pos,
                message: "unexpected end of query".into(),
            });
        }

        // Parenthesized expression
        if tokens[*pos] == Token::LParen {
            *pos += 1;
            let inner = self.parse_or(tokens, pos)?;
            if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
                return Err(QueryError::Parse {
                    position: *pos,
                    message: "expected closing parenthesis".into(),
                });
            }
            *pos += 1;
            return Ok(inner);
        }

        // Identifier — could be a tag or an attribute query
        if let Token::Ident(name) = &tokens[*pos] {
            let name = name.clone();
            *pos += 1;

            // Check for operator: = != < <= > >=
            if *pos < tokens.len() {
                if let Some(op) = match_op(&tokens[*pos]) {
                    *pos += 1;
                    let value = self.parse_value(tokens, pos)?;
                    let key = self.resolve_tag(&name)?;
                    return Ok(Query::HasAttr { key, op, value });
                }
                // key:value shorthand (e.g., "year:2024")
                if tokens[*pos] == Token::Colon {
                    *pos += 1;
                    let value = self.parse_value(tokens, pos)?;
                    let key = self.resolve_tag(&name)?;
                    return Ok(Query::HasAttr {
                        key,
                        op: CmpOp::Eq,
                        value,
                    });
                }
            }

            // Plain tag query
            let tag_id = self.resolve_tag(&name)?;
            return Ok(Query::HasTag(tag_id));
        }

        Err(QueryError::Parse {
            position: *pos,
            message: format!("unexpected token: {:?}", tokens[*pos]),
        })
    }

    fn parse_value(&self, tokens: &[Token], pos: &mut usize) -> Result<Value, QueryError> {
        if *pos >= tokens.len() {
            return Err(QueryError::Parse {
                position: *pos,
                message: "expected value".into(),
            });
        }

        match &tokens[*pos] {
            Token::Ident(s) => {
                *pos += 1;
                // Try parsing as integer
                if let Ok(n) = s.parse::<i64>() {
                    return Ok(Value::Int(n));
                }
                // Try parsing as float
                if let Ok(f) = s.parse::<f64>() {
                    return Ok(Value::Float(f));
                }
                Ok(Value::Text(s.clone()))
            }
            Token::Str(s) => {
                *pos += 1;
                Ok(Value::Text(s.clone()))
            }
            _ => Err(QueryError::Parse {
                position: *pos,
                message: "expected value".into(),
            }),
        }
    }

    fn resolve_tag(&self, name: &str) -> Result<TagId, QueryError> {
        self.dag
            .lookup(name)
            .ok_or_else(|| QueryError::UnknownTag(name.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    And,
    Or,
    Not,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Colon,
    LParen,
    RParen,
}

fn match_op(token: &Token) -> Option<CmpOp> {
    match token {
        Token::Eq => Some(CmpOp::Eq),
        Token::Ne => Some(CmpOp::Ne),
        Token::Lt => Some(CmpOp::Lt),
        Token::Le => Some(CmpOp::Le),
        Token::Gt => Some(CmpOp::Gt),
        Token::Ge => Some(CmpOp::Ge),
        _ => None,
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, QueryError> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Skip whitespace
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Operators
        match bytes[i] {
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            b':' => {
                tokens.push(Token::Colon);
                i += 1;
            }
            b'=' => {
                tokens.push(Token::Eq);
                i += 1;
            }
            b'!' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::Ne);
                i += 2;
            }
            b'<' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::Le);
                i += 2;
            }
            b'<' => {
                tokens.push(Token::Lt);
                i += 1;
            }
            b'>' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::Ge);
                i += 2;
            }
            b'>' => {
                tokens.push(Token::Gt);
                i += 1;
            }
            b'"' => {
                // Quoted string
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(QueryError::Parse {
                        position: start - 1,
                        message: "unterminated string".into(),
                    });
                }
                let s = String::from_utf8_lossy(&bytes[start..i]).to_string();
                tokens.push(Token::Str(s));
                i += 1; // skip closing quote
            }
            _ if bytes[i].is_ascii_alphanumeric()
                || bytes[i] == b'_'
                || bytes[i] == b'-'
                || bytes[i] == b'.' =>
            {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || bytes[i] == b'-'
                        || bytes[i] == b'.')
                {
                    i += 1;
                }
                let word = String::from_utf8_lossy(&bytes[start..i]).to_string();
                match word.as_str() {
                    "AND" | "and" => tokens.push(Token::And),
                    "OR" | "or" => tokens.push(Token::Or),
                    "NOT" | "not" => tokens.push(Token::Not),
                    _ => tokens.push(Token::Ident(word)),
                }
            }
            _ => {
                return Err(QueryError::Parse {
                    position: i,
                    message: format!("unexpected character: {}", bytes[i] as char),
                });
            }
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_ontology::{TagDefinition, TagSemantics},
    };

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn setup_dag() -> ImplicationDag {
        let mut dag = ImplicationDag::new();
        dag.register_tag(TagDefinition::new(
            tag(1),
            "electronic",
            TagSemantics::Label,
        ))
        .unwrap();
        dag.register_tag(TagDefinition::new(tag(2), "portable", TagSemantics::Label))
            .unwrap();
        dag.register_tag(TagDefinition::new(
            tag(3),
            "discontinued",
            TagSemantics::Label,
        ))
        .unwrap();
        dag.register_tag(TagDefinition::new(
            tag(10),
            "year",
            TagSemantics::Attribute {
                value_type: mimisbrunnr_ontology::ValueType::Int,
            },
        ))
        .unwrap();
        dag.register_tag(TagDefinition::new(
            tag(11),
            "project",
            TagSemantics::Attribute {
                value_type: mimisbrunnr_ontology::ValueType::Text,
            },
        ))
        .unwrap();
        dag.register_tag(TagDefinition::new(tag(12), "source", TagSemantics::Label))
            .unwrap();
        dag.register_tag(TagDefinition::new(
            tag(13),
            "lang",
            TagSemantics::Attribute {
                value_type: mimisbrunnr_ontology::ValueType::Text,
            },
        ))
        .unwrap();
        dag
    }

    #[test]
    fn parse_simple_tag() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("electronic").unwrap();
        assert_eq!(q, Query::HasTag(tag(1)));
    }

    #[test]
    fn parse_and() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("electronic AND portable").unwrap();
        assert_eq!(
            q,
            Query::And(vec![Query::HasTag(tag(1)), Query::HasTag(tag(2))])
        );
    }

    #[test]
    fn parse_or() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("electronic OR portable").unwrap();
        assert_eq!(
            q,
            Query::Or(vec![Query::HasTag(tag(1)), Query::HasTag(tag(2))])
        );
    }

    #[test]
    fn parse_not() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("NOT discontinued").unwrap();
        assert_eq!(q, Query::Not(Box::new(Query::HasTag(tag(3)))));
    }

    #[test]
    fn parse_attr_eq() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("year=2024").unwrap();
        assert_eq!(
            q,
            Query::HasAttr {
                key: tag(10),
                op: CmpOp::Eq,
                value: Value::Int(2024),
            }
        );
    }

    #[test]
    fn parse_attr_colon_shorthand() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("year:2024").unwrap();
        assert_eq!(
            q,
            Query::HasAttr {
                key: tag(10),
                op: CmpOp::Eq,
                value: Value::Int(2024),
            }
        );
    }

    #[test]
    fn parse_complex_query() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser
            .parse("electronic AND portable AND year:2024 AND NOT discontinued")
            .unwrap();

        match q {
            Query::And(parts) => {
                assert_eq!(parts.len(), 4);
                assert_eq!(parts[0], Query::HasTag(tag(1)));
                assert_eq!(parts[1], Query::HasTag(tag(2)));
                assert!(matches!(parts[2], Query::HasAttr { .. }));
                assert!(matches!(parts[3], Query::Not(_)));
            }
            _ => panic!("expected And"),
        }
    }

    #[test]
    fn parse_project_query() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser
            .parse("project=vesper AND source AND lang=rust")
            .unwrap();

        match q {
            Query::And(parts) => {
                assert_eq!(parts.len(), 3);
            }
            _ => panic!("expected And"),
        }
    }

    #[test]
    fn parse_parenthesized() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser
            .parse("(electronic OR portable) AND NOT discontinued")
            .unwrap();

        match q {
            Query::And(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[0], Query::Or(_)));
                assert!(matches!(&parts[1], Query::Not(_)));
            }
            _ => panic!("expected And"),
        }
    }

    #[test]
    fn parse_quoted_string_value() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser.parse("project=\"vesper kernel\"").unwrap();
        assert_eq!(
            q,
            Query::HasAttr {
                key: tag(11),
                op: CmpOp::Eq,
                value: Value::Text("vesper kernel".into()),
            }
        );
    }

    #[test]
    fn parse_unknown_tag_error() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let err = parser.parse("nonexistent").unwrap_err();
        assert!(matches!(err, QueryError::UnknownTag(_)));
    }

    #[test]
    fn parse_empty_error() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        assert!(matches!(parser.parse(""), Err(QueryError::EmptyQuery)));
    }

    #[test]
    fn parse_case_insensitive_keywords() {
        let dag = setup_dag();
        let parser = QueryParser::new(&dag);
        let q = parser
            .parse("electronic and portable or discontinued")
            .unwrap();
        // "electronic AND portable" has higher precedence than OR
        assert!(matches!(q, Query::Or(_)));
    }

    #[test]
    fn tokenize_operators() {
        let tokens = tokenize("a != b <= c >= d < e > f").unwrap();
        assert!(tokens.contains(&Token::Ne));
        assert!(tokens.contains(&Token::Le));
        assert!(tokens.contains(&Token::Ge));
        assert!(tokens.contains(&Token::Lt));
        assert!(tokens.contains(&Token::Gt));
    }
}
