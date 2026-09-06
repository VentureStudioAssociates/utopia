//! OIS governed kernel — spike port of the OIS TypeScript oracle semantics
//! beside Utopia's fact model, as additive side tables and pure functions.
//!
//! Ported from the frozen TypeScript oracle at
//! `savvytinker-second-brain @ frontier/ois-v1 c56f8e5`:
//! - `packages/core/src/resolver/resolve.ts` → [`resolver`]
//! - `packages/core/src/governance/{profile,events,write}.ts` → [`governance`]
//! - `packages/work/src/promotion.ts` → [`promotion`]
//!
//! Purity contract (mirrors the originals): identical inputs give identical
//! outputs; no clocks, no I/O, no store reads inside the resolution or write
//! logic. Persistence sits behind the [`store::EnvelopeStore`] trait.
//! Permission limits fail closed: a non-granted basis never discloses
//! content, and evidence sufficiency never grants authority.
//!
//! Vocabulary is frozen by the OIS v1 contracts; lifecycle states and
//! authority classes stay distinct exactly as in TypeScript.

pub mod canonical;
pub mod envelope;
pub mod governance;
pub mod ingest;
pub mod promotion;
pub mod resolver;
pub mod store;

pub use envelope::{AuthorityClass, KernelEnvelope, LifecycleState, ObjectType};
pub use governance::{
    apply_governed_write, classify_change_class, evaluate_capability, is_stale_base,
    GovernedWriteCommand, GovernedWriteOutcome, PrincipalBasis, Profile, RecordedOutcome,
};
pub use resolver::{
    resolve_current_state, Currentness, NotEstablishedBasis, ResolutionCorpus,
    ResolutionDiagnostic, ResolutionQuery, ResolverInputError,
};
