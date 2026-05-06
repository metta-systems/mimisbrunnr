//! Path-attribute construction (DESIGN §12.3, IMPL §10.3, §4.3).
//!
//! On-disk, a per-context Unix path is stored as the assertion
//! `Attr(unix-path, Value::Scoped { context, inner: Value::Text(path) })`.
//!
//! The `unix-path` tag id and the `unix-path-context:*` Grouping tag id are
//! *resolved by the caller* against the live ontology — this crate doesn't
//! carry an ontology handle, so it can't do the resolution itself. We expose
//! [`UNIX_PATH_TAG_NAME`] as the canonical name so all callers agree.

use mimisbrunnr_types::{Assertion, TagId, Value};

use crate::error::UnixError;
use crate::projection::validate_relative_path;

/// Canonical name of the `unix-path` attribute tag in the `unix-interop`
/// ontology module (DESIGN §12.2). Engine layers resolve this name to a
/// concrete [`TagId`] before calling [`build_path_attr`].
pub const UNIX_PATH_TAG_NAME: &str = "unix-path";

/// Build the [`Assertion`] that records `path` as the unix-path of an object
/// under `context`. The caller passes in the resolved `unix_path_tag` id
/// (looked up via ontology) and the `context` Grouping tag.
///
/// The `path` string is validated as a relative, NUL-free, non-traversing
/// path before being wrapped in the assertion.
pub fn build_path_attr(
    unix_path_tag: TagId,
    context: TagId,
    path: &str,
) -> Result<Assertion, UnixError> {
    validate_relative_path(path)?;
    let inner = Value::Text(path.to_string());
    let scoped =
        Value::scoped(context, inner).expect("inner is Text, never Scoped — cannot fail");
    Ok(Assertion::Attr {
        key: unix_path_tag,
        value: scoped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_path_attr_returns_scoped_text() {
        let unix_path = TagId::new(11);
        let ctx = TagId::new(42);
        let assertion = build_path_attr(unix_path, ctx, "boot/kernel8.img").unwrap();
        match assertion {
            Assertion::Attr { key, value } => {
                assert_eq!(key, unix_path);
                match value {
                    Value::Scoped { context, inner } => {
                        assert_eq!(context, ctx);
                        match *inner {
                            Value::Text(s) => assert_eq!(s, "boot/kernel8.img"),
                            other => panic!("expected Text, got {other:?}"),
                        }
                    }
                    other => panic!("expected Scoped, got {other:?}"),
                }
            }
            other => panic!("expected Attr, got {other:?}"),
        }
    }

    #[test]
    fn build_path_attr_rejects_absolute() {
        let err =
            build_path_attr(TagId::new(1), TagId::new(2), "/absolute/path").unwrap_err();
        assert!(matches!(err, UnixError::AbsolutePath(_)));
    }

    #[test]
    fn unix_path_name_is_stable() {
        assert_eq!(UNIX_PATH_TAG_NAME, "unix-path");
    }
}
