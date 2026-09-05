//! Kernel envelope model — the Rust mirror of the frozen OIS v1 contract
//! `Kernel.OISSemanticKernelV1` (packages/contracts/schemas/v1/kernel.schema.json).
//!
//! Field names and the vocabulary enums are frozen: they serialize to exactly
//! the TypeScript oracle's wire shapes, so a stored envelope round-trips into
//! either runtime. The four authority classes stay distinct, and the
//! candidate/working ↔ working_knowledge/candidate_knowledge coupling the
//! contract enforces is asserted by the resolver (never silently coerced).

use serde::{Deserialize, Serialize};

/// The frozen 12-object semantic kernel vocabulary.
pub type ObjectType = String;

/// The four frozen authority classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityClass {
    #[serde(rename = "protected_constraint")]
    ProtectedConstraint,
    #[serde(rename = "current_operating_state")]
    CurrentOperatingState,
    #[serde(rename = "working_knowledge")]
    WorkingKnowledge,
    #[serde(rename = "candidate_knowledge")]
    CandidateKnowledge,
}

impl AuthorityClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProtectedConstraint => "protected_constraint",
            Self::CurrentOperatingState => "current_operating_state",
            Self::WorkingKnowledge => "working_knowledge",
            Self::CandidateKnowledge => "candidate_knowledge",
        }
    }

    /// Governing-capable authority: lifecycle `governing`/`governing_but_disputed`
    /// is only coherent with these two classes.
    pub fn is_governing(&self) -> bool {
        matches!(
            self,
            Self::ProtectedConstraint | Self::CurrentOperatingState
        )
    }

    /// Candidate/working authority: lifecycle `candidate`/`working` is only
    /// coherent with these two classes.
    pub fn is_candidate_or_working(&self) -> bool {
        matches!(self, Self::WorkingKnowledge | Self::CandidateKnowledge)
    }
}

/// The seven frozen lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LifecycleState {
    #[serde(rename = "candidate")]
    Candidate,
    #[serde(rename = "working")]
    Working,
    #[serde(rename = "governing")]
    Governing,
    #[serde(rename = "governing_but_disputed")]
    GoverningButDisputed,
    #[serde(rename = "superseded")]
    Superseded,
    #[serde(rename = "retired")]
    Retired,
    #[serde(rename = "deprecated")]
    Deprecated,
}

impl LifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Working => "working",
            Self::Governing => "governing",
            Self::GoverningButDisputed => "governing_but_disputed",
            Self::Superseded => "superseded",
            Self::Retired => "retired",
            Self::Deprecated => "deprecated",
        }
    }

    /// The two governing-capable lifecycles. Disputes stay visible as their
    /// own state — never coerced to `governing`, never suppressed.
    pub fn is_governing(&self) -> bool {
        matches!(self, Self::Governing | Self::GoverningButDisputed)
    }

    /// Candidate/working lifecycles: they never govern and never displace a
    /// governing version.
    pub fn is_candidate_or_working(&self) -> bool {
        matches!(self, Self::Candidate | Self::Working)
    }
}

/// One kernel record: an envelope bound to exactly one governance root with
/// server-derived authority. Mirrors `Kernel.OISSemanticKernelV1` exactly —
/// including that unknown fields inside `object` are preserved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KernelEnvelope {
    #[serde(rename = "schema_version")]
    pub schema_version: String,

    pub object_type: ObjectType,

    pub object_id: String,

    #[serde(rename = "governance_root_ref")]
    pub governance_root_ref: String,

    #[serde(
        rename = "scope_refs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub scope_refs: Option<Vec<String>>,

    #[serde(rename = "authority_class")]
    pub authority_class: AuthorityClass,

    #[serde(rename = "lifecycle_state")]
    pub lifecycle_state: LifecycleState,

    #[serde(rename = "valid_at")]
    pub valid_at: Option<String>,

    #[serde(rename = "as_known_at")]
    pub as_known_at: String,

    #[serde(rename = "recorded_at")]
    pub recorded_at: String,

    #[serde(rename = "provenance_refs")]
    pub provenance_refs: Vec<String>,

    pub object: serde_json::Value,
}

impl KernelEnvelope {
    /// The `object_type:object_id` ref convention used across the oracle
    /// (core `kernelRef`). Pure.
    pub fn kernel_ref(&self) -> String {
        format!("{}:{}", self.object_type, self.object_id)
    }

    /// Value of `object.superseded_assertion_ref` when it is a string.
    pub fn superseded_assertion_ref(&self) -> Option<&str> {
        self.object
            .get("superseded_assertion_ref")
            .and_then(|v| v.as_str())
    }
}
