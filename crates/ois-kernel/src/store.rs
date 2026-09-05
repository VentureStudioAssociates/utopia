//! Persistence boundary for the governed kernel.
//!
//! The resolver, governed writes, and promotion are pure: they run over
//! envelopes handed to them and never touch a store. This trait is the seam
//! a storage layer implements (the spike wires it to the additive
//! `kernel_envelopes` / `governed_operations` side tables via
//! `utopia-store`); the kernel logic stays runnable over any record source.

use crate::envelope::KernelEnvelope;
use crate::governance::RecordedOutcome;

/// What persistence must provide for the governed kernel. All async-free —
/// implementors translate to their own storage semantics (e.g. sqlx).
pub trait EnvelopeStore {
    /// Appends one kernel envelope version. Envelopes are append-only:
    /// implementations must never rewrite an existing version.
    fn record_envelope(&self, envelope: &KernelEnvelope) -> Result<(), String>;

    /// Every recorded version of one object id (any recorded_at), any order —
    /// the resolver owns ordering.
    fn history_for_object(&self, object_id: &str) -> Result<Vec<KernelEnvelope>, String>;

    /// Records that may declare supersession of the target (candidate
    /// successors, related governing assertions). May return an empty set —
    /// supersession-by-declaration then only sees the history itself.
    fn related_records(&self, object_id: &str) -> Result<Vec<KernelEnvelope>, String>;

    /// Looks up a prior outcome by idempotency key, for replay detection.
    /// `Ok(None)` = no prior outcome (first attempt).
    fn prior_outcome(&self, idempotency_key: &str) -> Result<Option<RecordedOutcome>, String>;

    /// Records the outcome of a governed operation (any disposition) against
    /// its idempotency key — the replay guard's write side.
    fn record_operation(
        &self,
        idempotency_key: &str,
        outcome: &RecordedOutcome,
    ) -> Result<(), String>;
}
