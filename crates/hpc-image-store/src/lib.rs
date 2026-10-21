//! A Linux, user-owned filesystem OCI image store.
//! Synchronous transfers, inventory, graph checking, digest selection and removal.

#![doc = include_str!("../README.md")]

// The registry fixture is shared by unit and CLI integration tests.
#[cfg(test)]
extern crate self as hpc_image_store;

#[cfg(not(target_os = "linux"))]
compile_error!("hpc-image-store currently supports Linux only");

mod check;
mod error;
mod graph;
mod identity;
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
// Coordination also defines the operations reserved for later stages.
#[allow(dead_code)]
mod initialization;
#[allow(dead_code)]
mod layout;
#[allow(dead_code)]
mod locks;
mod metadata;
mod pull;
mod store;
mod subprocess;
mod transfer;

pub use check::{CheckDiagnostic, CheckEntry, CheckReport, CheckScope, CheckStatus};
pub use error::{Error, NamespaceChange, Result};
pub use identity::{ImageSelector, LocalRef, Sha256Digest};
pub use inventory::{
    DiagnosticCategory, ListDiagnostic, ListEntry, ListReport, ListStatus, RemoveReport,
};
pub use metadata::{Acquisition, ContentMetadata, ImageId, Platform, RefMetadata, StoreMetadata};
pub use presentation::format_list_podman;
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
