/// Attribute value types that can be attached to objects.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Int(i64),
    Float(f64),
    Timestamp(i64),
    Blob(Vec<u8>),
}

impl Value {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_timestamp(&self) -> Option<i64> {
        match self {
            Value::Timestamp(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Value::Blob(v) => Some(v),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Text(_) => "text",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Timestamp(_) => "timestamp",
            Value::Blob(_) => "blob",
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Text(s) => write!(f, "\"{s}\""),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Timestamp(v) => write!(f, "ts:{v}"),
            Value::Blob(v) => write!(f, "blob[{}]", v.len()),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Text(a), Value::Text(b)) => a.partial_cmp(b),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors() {
        assert_eq!(Value::Text("hello".into()).as_text(), Some("hello"));
        assert_eq!(Value::Int(42).as_int(), Some(42));
        assert_eq!(Value::Float(3.41).as_float(), Some(3.41));
        assert_eq!(Value::Timestamp(1000).as_timestamp(), Some(1000));
        assert_eq!(Value::Blob(vec![1, 2]).as_blob(), Some([1u8, 2].as_slice()));
    }

    #[test]
    fn wrong_accessor_returns_none() {
        assert_eq!(Value::Int(42).as_text(), None);
        assert_eq!(Value::Text("x".into()).as_int(), None);
    }

    #[test]
    fn type_names() {
        assert_eq!(Value::Text("".into()).type_name(), "text");
        assert_eq!(Value::Int(0).type_name(), "int");
        assert_eq!(Value::Float(0.0).type_name(), "float");
        assert_eq!(Value::Timestamp(0).type_name(), "timestamp");
        assert_eq!(Value::Blob(vec![]).type_name(), "blob");
    }

    #[test]
    fn ordering_same_type() {
        assert!(Value::Int(1) < Value::Int(2));
        assert!(Value::Text("a".into()) < Value::Text("b".into()));
    }

    #[test]
    fn ordering_different_types_is_none() {
        assert_eq!(Value::Int(1).partial_cmp(&Value::Text("a".into())), None);
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", Value::Int(42)), "42");
        assert_eq!(format!("{}", Value::Text("hi".into())), "\"hi\"");
        assert_eq!(format!("{}", Value::Blob(vec![1, 2, 3])), "blob[3]");
    }
}
