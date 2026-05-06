//! Parse a CLI-supplied attribute value into a [`Value`].

use mimisbrunnr::types::Value;

/// Hint provided by the user via `--type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Int,
    Float,
    Text,
    Timestamp,
    Blob,
    /// Try int → float → timestamp → text in that order.
    Auto,
}

impl ValueKind {
    /// Parse a `--type` flag string. Distinct from `FromStr` so the error
    /// type stays a `String` (instead of a custom enum).
    pub fn parse_kind(s: &str) -> Result<Self, String> {
        match s {
            "int" => Ok(Self::Int),
            "float" => Ok(Self::Float),
            "text" => Ok(Self::Text),
            "timestamp" | "ts" => Ok(Self::Timestamp),
            "blob" => Ok(Self::Blob),
            other => Err(format!("unknown value type {other:?}")),
        }
    }
}

/// Parse `raw` into a [`Value`], honouring the `--type` hint when set.
///
/// Auto inference order: Int → Float → Timestamp (ISO-8601 nanos) → Text.
/// `Blob` only ever produced when explicitly forced; the bytes are taken as
/// the raw UTF-8 of `raw`.
pub fn parse_value(raw: &str, kind: ValueKind) -> Result<Value, String> {
    match kind {
        ValueKind::Int => raw
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|e| format!("not an int: {e}")),
        ValueKind::Float => raw
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| format!("not a float: {e}")),
        ValueKind::Text => Ok(Value::Text(raw.to_string())),
        ValueKind::Timestamp => parse_timestamp(raw),
        ValueKind::Blob => Ok(Value::Blob(raw.as_bytes().to_vec())),
        ValueKind::Auto => Ok(infer(raw)),
    }
}

fn infer(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Int(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return Value::Float(f);
    }
    if let Ok(v) = parse_timestamp(raw) {
        return v;
    }
    Value::Text(raw.to_string())
}

/// Accepts a few timestamp forms:
///   - `ns:<i64>` raw nanoseconds since the Unix epoch.
///   - `s:<i64>`  whole seconds since the Unix epoch.
///   - bare `i64` ≥ 1_000_000_000_000_000_000 (i.e. plausible ns-since-epoch).
///
/// Phase 7a deliberately keeps this lightweight; full ISO-8601 lands when a
/// shared time crate is added to the workspace.
pub fn parse_timestamp(raw: &str) -> Result<Value, String> {
    if let Some(rest) = raw.strip_prefix("ns:") {
        return rest
            .parse::<i64>()
            .map(Value::Timestamp)
            .map_err(|e| format!("bad ns timestamp: {e}"));
    }
    if let Some(rest) = raw.strip_prefix("s:") {
        return rest
            .parse::<i64>()
            .map(|s| Value::Timestamp(s.saturating_mul(1_000_000_000)))
            .map_err(|e| format!("bad s timestamp: {e}"));
    }
    // Plausible ns-since-epoch heuristic: any integer >= 10^18 is treated as ns.
    if let Ok(i) = raw.parse::<i64>()
        && i.unsigned_abs() >= 1_000_000_000_000_000_000
    {
        return Ok(Value::Timestamp(i));
    }
    Err(format!("not a timestamp literal: {raw:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_int() {
        assert_eq!(parse_value("42", ValueKind::Auto).unwrap(), Value::Int(42));
    }

    #[test]
    fn auto_float() {
        assert_eq!(
            parse_value("1.5", ValueKind::Auto).unwrap(),
            Value::Float(1.5)
        );
    }

    #[test]
    fn auto_text_fallback() {
        assert_eq!(
            parse_value("Aphex", ValueKind::Auto).unwrap(),
            Value::Text("Aphex".into())
        );
    }

    #[test]
    fn forced_text() {
        assert_eq!(
            parse_value("42", ValueKind::Text).unwrap(),
            Value::Text("42".into())
        );
    }

    #[test]
    fn forced_int() {
        assert_eq!(parse_value("42", ValueKind::Int).unwrap(), Value::Int(42));
    }

    #[test]
    fn forced_blob() {
        match parse_value("hello", ValueKind::Blob).unwrap() {
            Value::Blob(b) => assert_eq!(b, b"hello"),
            v => panic!("expected Blob, got {v:?}"),
        }
    }

    #[test]
    fn timestamp_ns_form() {
        let v = parse_value("ns:1700000000000000000", ValueKind::Timestamp).unwrap();
        assert_eq!(v, Value::Timestamp(1_700_000_000_000_000_000));
    }

    #[test]
    fn timestamp_seconds_form() {
        let v = parse_value("s:1700000000", ValueKind::Timestamp).unwrap();
        assert_eq!(v, Value::Timestamp(1_700_000_000_000_000_000));
    }
}
