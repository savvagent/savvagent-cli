//! Routing layers. Phase 3 ships the router skeleton plus `@`-prefix
//! parsing; modality / rules / heuristics arrive in Phases 4-6.

pub mod legacy_model;
pub mod prefix;
pub mod router;

pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
pub use router::{RoutingDecision, RoutingOverride, RoutingReason};
