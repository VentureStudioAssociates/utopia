//! Context manifests — deterministic, auditable statements of exactly what
//! context an Activity runs with. Rust mirror of the frozen contract
//! `context-manifest.schema.json` (`ContextManifest.OISContextManifestV1`) at
//! the oracle pin (`second-brain @ frontier/ois-v1 c56f8e5`).
//!
//! Manifests never pretend to completeness: completeness is
//! `complete | partial_context | partial_permissions`, and any partial
//! manifest itemizes its omissions with an `omission_kind` (`permission`,
//! `staleness`, `depth_budget`, `unknown`), the omitted refs, and a
//! human-readable reason. A `partial_permissions` manifest must cite the
//! permission envelope that restricted it — permission gaps are always
//! disclosed as permission state, never silently absorbed into context
//! incompleteness. A complete manifest carries zero omissions. Identical
//! inputs recompile to the identical digest.
//!
//! There is no executable TS manifest compiler at the pin (the contract
//! schema is the compilation contract), so the differential check for this
//! surface validates compiled manifests against the frozen schema on both
//! engines; digest determinism is checked via the oracle's `canonicalDigest`.
//!
//! Also ports the per-Principal derivation-key semantics of
//! `packages/mcp/src/derivations.ts`: no semantic derivation reuse across
//! Principals — cache keys bind (principal_ref, permission basis digest,
//! governance root, query digest); one Principal's permission-limited view is
//! never served as another Principal's complete view.

use crate::canonical::canonical_digest;
use crate::envelope::KernelEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed error for incoherent manifest inputs — fail-closed, never adapted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "context manifest incoherent: {}", self.0)
    }
}

impl std::error::Error for ManifestError {}

/// Progressive disclosure depth (`minimal | standard | deep`).
pub type ManifestDepth = String;

/// Omission kinds (`permission | staleness | depth_budget | unknown`).
pub type OmissionKind = String;

/// Completeness vocabulary (`complete | partial_context | partial_permissions`).
pub type Completeness = String;

pub const DEPTHS: &[&str] = &["minimal", "standard", "deep"];
pub const OMISSION_KINDS: &[&str] = &["permission", "staleness", "depth_budget", "unknown"];

/// One honest omission: what was left out, why, and which refs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Omission {
    #[serde(rename = "omission_kind")]
    pub omission_kind: OmissionKind,

    #[serde(rename = "resource_refs")]
    pub resource_refs: Vec<String>,

    pub reason: String,
}

/// A compiled Context Manifest — the wire shape mirrors the frozen schema
/// exactly (field names, optionality, conditional requirements).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextManifest {
    #[serde(rename = "schema_version")]
    pub schema_version: String,

    #[serde(rename = "manifest_id")]
    pub manifest_id: String,

    #[serde(rename = "governance_root_ref")]
    pub governance_root_ref: String,

    #[serde(
        rename = "scope_refs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub scope_refs: Option<Vec<String>>,

    #[serde(rename = "activity_ref")]
    pub activity_ref: String,

    pub depth: ManifestDepth,

    #[serde(rename = "included_resource_refs")]
    pub included_resource_refs: Vec<String>,

    #[serde(
        rename = "included_checkpoint_refs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub included_checkpoint_refs: Option<Vec<String>>,

    #[serde(
        rename = "included_activity_refs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub included_activity_refs: Option<Vec<String>>,

    #[serde(rename = "omitted", default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Vec<Omission>>,

    pub completeness: Completeness,

    /// Required when completeness is `partial_permissions`: the permission
    /// envelope that restricted the traversal.
    #[serde(
        rename = "permission_envelope_ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_envelope_ref: Option<String>,

    #[serde(
        rename = "basis_checkpoint_refs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub basis_checkpoint_refs: Option<Vec<String>>,

    #[serde(
        rename = "basis_as_known_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub basis_as_known_at: Option<String>,

    #[serde(rename = "compiled_at")]
    pub compiled_at: String,

    /// Pins the full content: identical inputs recompile identically.
    #[serde(rename = "manifest_digest")]
    pub manifest_digest: String,
}

/// The deterministic completeness of a compiled manifest, derived from its
/// omission set: any permission omission makes the manifest
/// `partial_permissions` (permission gaps dominate and are disclosed as
/// permission state, never as generic context incompleteness); any other
/// omission makes it `partial_context`; none — `complete`.
fn derive_completeness(omissions: &[Omission]) -> Completeness {
    if omissions.iter().any(|o| o.omission_kind == "permission") {
        "partial_permissions".to_string()
    } else if omissions.is_empty() {
        "complete".to_string()
    } else {
        "partial_context".to_string()
    }
}

fn nonempty(value: &str, field: &str) -> Result<(), ManifestError> {
    if value.is_empty() {
        return Err(ManifestError(format!("{field} is empty")));
    }
    Ok(())
}

fn unique_nonempty_refs(refs: &[String], field: &str) -> Result<(), ManifestError> {
    for (index, reference) in refs.iter().enumerate() {
        nonempty(reference, &format!("{field}[{index}]"))?;
        if refs[..index].contains(reference) {
            return Err(ManifestError(format!(
                "{field}[{index}] duplicates an earlier ref: {reference}"
            )));
        }
    }
    Ok(())
}

/// Compile input. `compiled_at` is supplied by the caller (the kernel stays
/// clock-free); the manifest is a record of what was assembled at known-time.
pub struct CompileInput {
    pub manifest_id: String,
    pub governance_root_ref: String,
    pub scope_refs: Vec<String>,
    pub activity_ref: String,
    pub depth: ManifestDepth,
    pub included_resource_refs: Vec<String>,
    pub included_checkpoint_refs: Vec<String>,
    pub included_activity_refs: Vec<String>,
    pub omissions: Vec<Omission>,
    pub basis_checkpoint_refs: Vec<String>,
    pub basis_as_known_at: Option<String>,
    pub compiled_at: String,
}

/// Compiles one Context Manifest. Deterministic and pure: the same input
/// compiles to the identical digest. Fail-closed on every contract violation:
/// unknown depths or omission kinds, nonempty ref arrays with duplicates,
/// omissions without reasons, and the schema's conditional requirements
/// (partial ⇒ ≥1 omission; partial_permissions ⇒ a permission envelope ref;
/// complete ⇒ zero omissions).
pub fn compile_manifest(input: &CompileInput) -> Result<ContextManifest, ManifestError> {
    nonempty(&input.manifest_id, "manifest_id")?;
    nonempty(&input.governance_root_ref, "governance_root_ref")?;
    nonempty(&input.activity_ref, "activity_ref")?;
    if !DEPTHS.contains(&input.depth.as_str()) {
        return Err(ManifestError(format!(
            "depth is not a frozen depth: {}",
            input.depth
        )));
    }
    unique_nonempty_refs(&input.included_resource_refs, "included_resource_refs")?;
    unique_nonempty_refs(&input.included_checkpoint_refs, "included_checkpoint_refs")?;
    unique_nonempty_refs(&input.included_activity_refs, "included_activity_refs")?;
    unique_nonempty_refs(&input.basis_checkpoint_refs, "basis_checkpoint_refs")?;
    unique_nonempty_refs(&input.scope_refs, "scope_refs")?;

    for (index, omission) in input.omissions.iter().enumerate() {
        if !OMISSION_KINDS.contains(&omission.omission_kind.as_str()) {
            return Err(ManifestError(format!(
                "omitted[{index}].omission_kind is not a frozen omission kind: {}",
                omission.omission_kind
            )));
        }
        if omission.resource_refs.is_empty() {
            return Err(ManifestError(format!(
                "omitted[{index}].resource_refs is empty (min 1 per contract)"
            )));
        }
        unique_nonempty_refs(
            &omission.resource_refs,
            &format!("omitted[{index}].resource_refs"),
        )?;
        nonempty(&omission.reason, &format!("omitted[{index}].reason"))?;
    }

    // Permission-envelope provenance must be carried by the caller when a
    // permission omission is disclosed; the schema requires it on
    // partial_permissions and the compiler refuses to invent it.
    let permission_envelope_ref = input
        .omissions
        .iter()
        .find(|o| o.omission_kind == "permission")
        .and_then(|o| o.resource_refs.first())
        .map(|reference| format!("permission_envelope:{reference}"));
    let completeness = derive_completeness(&input.omissions);
    if completeness == "partial_permissions" && permission_envelope_ref.is_none() {
        return Err(ManifestError(
            "partial_permissions manifest must cite the permission envelope that restricted it"
                .to_string(),
        ));
    }

    let omitted = if input.omissions.is_empty() {
        None
    } else {
        Some(input.omissions.clone())
    };

    let mut manifest = ContextManifest {
        schema_version: "1.0.0".to_string(),
        manifest_id: input.manifest_id.clone(),
        governance_root_ref: input.governance_root_ref.clone(),
        scope_refs: if input.scope_refs.is_empty() {
            None
        } else {
            Some(input.scope_refs.clone())
        },
        activity_ref: input.activity_ref.clone(),
        depth: input.depth.clone(),
        included_resource_refs: input.included_resource_refs.clone(),
        included_checkpoint_refs: if input.included_checkpoint_refs.is_empty() {
            None
        } else {
            Some(input.included_checkpoint_refs.clone())
        },
        included_activity_refs: if input.included_activity_refs.is_empty() {
            None
        } else {
            Some(input.included_activity_refs.clone())
        },
        omitted,
        completeness: completeness.clone(),
        permission_envelope_ref: permission_envelope_ref.clone(),
        basis_checkpoint_refs: if input.basis_checkpoint_refs.is_empty() {
            None
        } else {
            Some(input.basis_checkpoint_refs.clone())
        },
        basis_as_known_at: input.basis_as_known_at.clone(),
        compiled_at: input.compiled_at.clone(),
        manifest_digest: String::new(),
    };

    // Digest over the manifest without its own digest — identical inputs
    // (including the compiled_at instant) recompile identically.
    manifest.manifest_digest = String::new();
    let value = serde_json::to_value(&manifest)
        .map_err(|_| ManifestError("manifest is not serializable".to_string()))?;
    manifest.manifest_digest = canonical_digest(&value);
    Ok(manifest)
}

/// A permission basis view for derivation keys — the resolver's
/// `PermissionBasis` projection (`decision`, `disclosure_state`,
/// `withheld_count`) that shapes every derivation.
pub struct PermissionBasisView {
    pub decision: String,
    pub disclosure_state: String,
    pub withheld_count: Option<i64>,
}

/// Stable digest of a permission basis for derivation keys — mirrors
/// `permissionDigest` in `derivations.ts` (canonical digest of the basis, or
/// of `null` when no basis applies).
pub fn permission_digest(basis: Option<&PermissionBasisView>) -> String {
    let value: Value = match basis {
        None => Value::Null,
        Some(basis) => serde_json::json!({
            "decision": basis.decision,
            "disclosure_state": basis.disclosure_state,
            "withheld_count": basis.withheld_count,
        }),
    };
    canonical_digest(&value)
}

/// The derivation cache key: `(principal_ref, permission_digest,
/// governance_root, query_digest)`. A lookup under any different owner is
/// structurally a miss — one Principal's permission-limited view is never
/// served as another's complete view, and the same human's two Principals
/// never collapse into one derivation owner.
pub fn derivation_key(
    principal_ref: &str,
    basis_digest: &str,
    governance_root_ref: &str,
    query: &Value,
) -> String {
    // canonical_digest canonicalizes before hashing, so key order and
    // whitespace in the query object cannot split one derivation in two.
    let query_digest = canonical_digest(query);
    format!("{principal_ref}|{basis_digest}|{governance_root_ref}|{query_digest}")
}

/// The permission envelope ref a manifest cites when a permission omission is
/// disclosed — derived from the kernel envelope that restricted the view
/// (`permission_envelope:{kernel ref}`), never invented.
pub fn permission_envelope_ref_for(restricting: &KernelEnvelope) -> String {
    format!("permission_envelope:{}", restricting.kernel_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn omission(kind: &str, refs: &[&str], reason: &str) -> Omission {
        Omission {
            omission_kind: kind.to_string(),
            resource_refs: refs.iter().map(|s| s.to_string()).collect(),
            reason: reason.to_string(),
        }
    }

    fn input(omissions: Vec<Omission>) -> CompileInput {
        CompileInput {
            manifest_id: "man-1".to_string(),
            governance_root_ref: "governance_root:gentle-roost".to_string(),
            scope_refs: vec!["scope:ops".to_string()],
            activity_ref: "activity:act-1".to_string(),
            depth: "standard".to_string(),
            included_resource_refs: vec!["assertion:asrt-line-speed".to_string()],
            included_checkpoint_refs: vec!["checkpoint:cp-1".to_string()],
            included_activity_refs: vec![],
            omissions,
            basis_checkpoint_refs: vec!["checkpoint:cp-1".to_string()],
            basis_as_known_at: Some("2026-09-05T00:00:00Z".to_string()),
            compiled_at: "2026-09-05T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn complete_manifest_has_no_omissions_and_stable_digest() {
        let first = compile_manifest(&input(vec![])).unwrap();
        let again = compile_manifest(&input(vec![])).unwrap();
        assert_eq!(first.completeness, "complete");
        assert!(first.omitted.is_none());
        assert_eq!(first.manifest_digest, again.manifest_digest);
        // A different compiled_at changes the digest (the record pins its
        // known-time basis, not a wish list).
        let mut later = input(vec![]);
        later.compiled_at = "2026-09-05T11:00:00Z".to_string();
        assert_ne!(
            first.manifest_digest,
            compile_manifest(&later).unwrap().manifest_digest
        );
    }

    #[test]
    fn depth_budget_omission_is_partial_context() {
        let manifest = compile_manifest(&input(vec![omission(
            "depth_budget",
            &["assertion:asrt-x"],
            "deep traversal exceeded the standard depth budget",
        )]))
        .unwrap();
        assert_eq!(manifest.completeness, "partial_context");
        assert_eq!(manifest.omitted.as_ref().unwrap().len(), 1);
        assert!(manifest.permission_envelope_ref.is_none());
    }

    #[test]
    fn permission_omission_requires_and_carries_the_envelope_ref() {
        let manifest = compile_manifest(&input(vec![omission(
            "permission",
            &["assertion:asrt-withheld"],
            "withheld for this principal's basis",
        )]))
        .unwrap();
        assert_eq!(manifest.completeness, "partial_permissions");
        assert_eq!(
            manifest.permission_envelope_ref.as_deref(),
            Some("permission_envelope:assertion:asrt-withheld")
        );
    }

    #[test]
    fn permission_dominates_mixed_omissions() {
        let manifest = compile_manifest(&input(vec![
            omission("depth_budget", &["assertion:asrt-x"], "budget"),
            omission("permission", &["assertion:asrt-y"], "withheld"),
        ]))
        .unwrap();
        assert_eq!(manifest.completeness, "partial_permissions");
        assert_eq!(manifest.omitted.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn contract_violations_fail_closed() {
        // complete with omissions is impossible by derivation; the schema's
        // other guards surface as errors here.
        let mut bad = input(vec![]);
        bad.depth = "infinite".to_string();
        assert_eq!(
            compile_manifest(&bad).unwrap_err().0,
            "depth is not a frozen depth: infinite"
        );

        let mut bad = input(vec![]);
        bad.included_resource_refs = vec!["a".to_string(), "a".to_string()];
        assert!(compile_manifest(&bad)
            .unwrap_err()
            .0
            .starts_with("included_resource_refs[1]"));

        let bad = input(vec![omission("mystery", &["r"], "because")]);
        assert!(compile_manifest(&bad)
            .unwrap_err()
            .0
            .starts_with("omitted[0].omission_kind"));

        let bad = input(vec![omission("permission", &[], "withheld")]);
        assert_eq!(
            compile_manifest(&bad).unwrap_err().0,
            "omitted[0].resource_refs is empty (min 1 per contract)"
        );

        let bad = input(vec![omission("staleness", &["r"], "")]);
        assert!(compile_manifest(&bad)
            .unwrap_err()
            .0
            .starts_with("omitted[0].reason"));
    }

    #[test]
    fn derivation_keys_never_cross_principals_or_permission_bases() {
        let query = json!({"state": "governing", "as_known_at": "2026-09-05T00:00:00Z"});
        let full = PermissionBasisView {
            decision: "granted".to_string(),
            disclosure_state: "fully_visible".to_string(),
            withheld_count: None,
        };
        let limited = PermissionBasisView {
            decision: "permission_limited".to_string(),
            disclosure_state: "restricted_existence_count_only".to_string(),
            withheld_count: Some(1),
        };

        let key_a_full = derivation_key(
            "principal:a",
            &permission_digest(Some(&full)),
            "governance_root:gentle-roost",
            &query,
        );
        let key_b_full = derivation_key(
            "principal:b",
            &permission_digest(Some(&full)),
            "governance_root:gentle-roost",
            &query,
        );
        let key_a_limited = derivation_key(
            "principal:a",
            &permission_digest(Some(&limited)),
            "governance_root:gentle-roost",
            &query,
        );
        let key_no_basis = derivation_key(
            "principal:a",
            &permission_digest(None),
            "governance_root:gentle-roost",
            &query,
        );

        // A different Principal is structurally a different owner.
        assert_ne!(key_a_full, key_b_full);
        // A different permission basis is a different owner — the same human's
        // limited view is never served as their complete view.
        assert_ne!(key_a_full, key_a_limited);
        // No-basis derivations have their own stable key.
        assert_ne!(key_a_full, key_no_basis);
        // Identical inputs recompute identical keys.
        assert_eq!(
            key_a_full,
            derivation_key(
                "principal:a",
                &permission_digest(Some(&full)),
                "governance_root:gentle-roost",
                &query
            )
        );
    }
}
