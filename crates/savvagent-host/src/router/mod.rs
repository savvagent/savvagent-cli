//! Routing layers. Phase 3 ships the router skeleton plus `@`-prefix
//! parsing; modality / rules / heuristics arrive in Phases 4-6.

pub mod legacy_model;
pub mod namespace;
pub mod prefix;
// The `router` submodule is the home of the `Router` struct (lands in
// Task 5) plus the routing-decision data types. Naming it `router`
// inside `router/` mirrors the public re-export path callers use
// (`savvagent_host::Router`) and keeps the file structure honest about
// what it owns.
#[allow(clippy::module_inception)]
pub mod router;

pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
pub use router::{Router, RoutingDecision, RoutingOverride, RoutingReason};
