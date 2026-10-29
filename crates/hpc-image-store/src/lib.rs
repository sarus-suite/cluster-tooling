//! A Linux, user-owned filesystem OCI image store.
//! Synchronous transfers, inventory, graph checking, local availability,
//! digest selection, removal and conservative pruning.

#![doc = include_str!("../docs/library-guide.md")]

// The registry fixture is shared by unit and CLI integration tests.
#[cfg(test)]
extern crate self as hpc_image_store;

#[cfg(not(target_os = "linux"))]
compile_error!("hpc-image-store currently supports Linux only");

mod availability;
mod check;
mod error;
mod graph;
mod identity;
mod inspection;
mod inventory;
pub mod presentation;
// Storage primitives are internal; transfers own their locking and publication.
#[allow(dead_code)]
mod cas;
#[allow(dead_code)]
mod discovery;
#[allow(dead_code)]
mod fsutil;
#[allow(dead_code)]
mod oci;
#[allow(dead_code)]
mod refs;
#[allow(dead_code)]
mod staging;
// Coordination guards retain locks for each operation's use of store data.
#[allow(dead_code)]
mod initialization;
#[allow(dead_code)]
mod layout;
#[allow(dead_code)]
mod locks;
mod metadata;
mod prune;
mod pull;
mod store;
mod subprocess;
mod transfer;

pub use availability::{AvailabilityDiagnostic, AvailabilityDiagnosticKind, ImageAvailability};
pub use check::{CheckDiagnostic, CheckEntry, CheckReport, CheckScope, CheckStatus};
pub use error::{Error, NamespaceChange, Result};
pub use identity::{ImageSelector, LocalRef, Sha256Digest};
pub use inventory::{
    DiagnosticCategory, ListDiagnostic, ListEntry, ListReport, ListStatus, RemoveReport,
};
pub use metadata::{Acquisition, ContentMetadata, ImageId, Platform, RefMetadata, StoreMetadata};
pub use presentation::format_list_podman;
pub use prune::{
    PruneCandidate, PruneDiagnostic, PruneKind, PruneOptions, PrunePhase, PruneReport,
    PruneSkipped, PruneSummary,
};
pub use pull::{PullContext, PullOptions, PullReport};
pub use staging::CleanupWarning;
pub use store::{Store, StoreConfig};
pub use transfer::{ExportReport, ImportOptions, ImportReport, TransferContext};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod publication_tests;

#[cfg(test)]
#[path = "../tests/support/snapshot.rs"]
mod test_snapshot;
