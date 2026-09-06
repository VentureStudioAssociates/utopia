//! Canonical JSON serialization and SHA-256 content digests for
//! deterministic governance identity (churn keys, event ids, accepted
//! snapshot digests).
//!
//! Byte-for-byte port of `packages/core/src/canonical.ts` (`canonicalJson`,
//! `canonicalDigest`): sorted keys, stable formatting. The sort is by UTF-16
//! code unit (JavaScript `<` order), not by code point — identical on ASCII,
//! and implemented explicitly here so non-ASCII keys also agree with the
//! oracle. Numbers: both runtimes emit shortest round-trip formatting for the
//! integers and short decimals our corpora use; exotic float exponents are a
//! documented boundary (see SPIKE-REPORT.md).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Compare two strings by UTF-16 code unit order (JavaScript string `<`).
fn js_str_lt(a: &str, b: &str) -> bool {
    let mut ai = a.encode_utf16();
    let mut bi = b.encode_utf16();
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return false,
            (None, Some(_)) => return true,
            (Some(_), None) => return false,
            (Some(ac), Some(bc)) => {
                if ac != bc {
                    return ac < bc;
                }
            }
        }
    }
}

/// Canonical JSON: sorted keys, stable formatting, no undefined values. Pure.
/// (`undefined` cannot exist in `serde_json::Value`; JSON `null` serializes
/// as `null` exactly as in TypeScript.)
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| {
                if js_str_lt(a, b) {
                    std::cmp::Ordering::Less
                } else if js_str_lt(b, a) {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            });
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// SHA-256 hex digest of the canonical JSON serialization. Pure and total.
pub fn canonical_digest(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(value).as_bytes());
    let out = hasher.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for byte in out {
        hex.push_str(&format!("{:02x}", byte));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_form_matches_ts_examples() {
        // Sorted keys, arrays in order, null kept.
        let v = json!({"b": 1, "a": [1, 2, {"z": null, "y": true}], "c": "s"});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":[1,2,{"y":true,"z":null}],"b":1,"c":"s"}"#
        );

        // Nested object sort and string escaping.
        let v = json!({"k": {"a": "x\"y"}});
        assert_eq!(canonical_json(&v), r#"{"k":{"a":"x\"y"}}"#);

        // Digest is stable hex over the canonical form.
        let d = canonical_digest(&json!({"a": 1}));
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
