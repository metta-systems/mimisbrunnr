//! Path-attribute construction (SPECIFICATION-DESIGN§12.3, UNIX-DESIGN§1).
//!
//! An object's Unix path is stored as the assertion
//! `Attr(unix-path, Value::Text(path))`.
//!
//! The `unix-path` tag id is *resolved by the caller* against the live
//! ontology — this crate doesn't carry an ontology handle, so it can't do the
//! resolution itself. We expose [`UNIX_PATH_TAG_NAME`] as the canonical name
//! so all callers agree.

use mimisbrunnr_types::{Assertion, TagId, Value};

use crate::error::UnixError;
use crate::projection::validate_relative_path;

/// Canonical name of the `unix-path` attribute tag in the `unix-interop`
/// ontology module (SPECIFICATION-DESIGN§12.2). Engine layers resolve this name to a
/// concrete [`TagId`] before calling [`build_path_attr`].
pub const UNIX_PATH_TAG_NAME: &str = "unix-path";

/// Build the [`Assertion`] that records `path` as a unix-path of an object.
/// The caller passes in the resolved `unix_path_tag` id (looked up via
/// ontology).
///
/// The `path` string is validated as a relative, NUL-free, non-traversing
/// path before being wrapped in the assertion.
pub fn build_path_attr(unix_path_tag: TagId, path: &str) -> Result<Assertion, UnixError> {
    validate_relative_path(path)?;
    Ok(Assertion::Attr {
        key: unix_path_tag,
        value: Value::Text(path.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_path_attr_returns_text() {
        let unix_path = TagId::new(11);
        let assertion = build_path_attr(unix_path, "boot/kernel8.img").unwrap();
        match assertion {
            Assertion::Attr { key, value } => {
                assert_eq!(key, unix_path);
                assert_eq!(value, Value::Text("boot/kernel8.img".into()));
            }
            other => panic!("expected Attr, got {other:?}"),
        }
    }

    #[test]
    fn build_path_attr_rejects_absolute() {
        let err = build_path_attr(TagId::new(1), "/absolute/path").unwrap_err();
        assert!(matches!(err, UnixError::AbsolutePath(_)));
    }

    #[test]
    fn unix_path_name_is_stable() {
        assert_eq!(UNIX_PATH_TAG_NAME, "unix-path");
    }
}
