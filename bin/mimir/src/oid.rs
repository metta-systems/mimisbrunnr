//! `obj:<oid>` parsing helpers.

use mimisbrunnr::types::ObjectId;

/// Parse an `obj:<n>` reference from the CLI.
///
/// Accepts:
///   - `obj:42`            decimal local id (node = 0)
///   - `obj:0x2a`          hex local id (node = 0)
///   - `obj:<node>:<local>` engine-display form (node hex, local decimal)
pub fn parse_oid(s: &str) -> Result<ObjectId, String> {
    let body = s
        .strip_prefix("obj:")
        .ok_or_else(|| format!("expected obj:<id>, got {s:?}"))?;

    // engine-display form `node:local`?
    if let Some((node_part, local_part)) = body.split_once(':') {
        let node = u16::from_str_radix(node_part, 16)
            .map_err(|e| format!("bad node id {node_part:?}: {e}"))?;
        let local: u64 = local_part
            .parse()
            .map_err(|e| format!("bad local id {local_part:?}: {e}"))?;
        return Ok(ObjectId::from_parts(node, local));
    }

    let local = if let Some(hex) = body.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("bad hex id {hex:?}: {e}"))?
    } else {
        body.parse::<u64>()
            .map_err(|e| format!("bad oid {body:?}: {e}"))?
    };
    Ok(ObjectId::from_parts(0, local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decimal() {
        let oid = parse_oid("obj:42").unwrap();
        assert_eq!(oid.local_seq(), 42);
        assert_eq!(oid.node_id(), 0);
    }

    #[test]
    fn parse_hex() {
        let oid = parse_oid("obj:0x2a").unwrap();
        assert_eq!(oid.local_seq(), 42);
    }

    #[test]
    fn parse_full_form() {
        let oid = parse_oid("obj:1:42").unwrap();
        assert_eq!(oid.local_seq(), 42);
        assert_eq!(oid.node_id(), 1);
    }

    #[test]
    fn parse_full_form_hex_node() {
        let oid = parse_oid("obj:abcd:42").unwrap();
        assert_eq!(oid.local_seq(), 42);
        assert_eq!(oid.node_id(), 0xabcd);
    }

    #[test]
    fn parse_rejects_bad_prefix() {
        assert!(parse_oid("42").is_err());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_oid("obj:not-a-number").is_err());
    }
}
