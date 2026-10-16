//! Foundations for a Linux, user-owned filesystem OCI image store.
//! Synchronous containers-storage import and export using private OCI staging.

#![doc = include_str!("../README.md")]

#[cfg(not(target_os = "linux"))]
compile_error!("hpc-image-store currently supports Linux only");

mod error;
mod identity;
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
mod store;
mod subprocess;
mod transfer;

pub use error::{Error, NamespaceChange, Result};
pub use identity::{LocalRef, Sha256Digest};
pub use metadata::{Acquisition, ContentMetadata, ImageId, Platform, RefMetadata, StoreMetadata};
pub use staging::CleanupWarning;
pub use store::{Store, StoreConfig};
pub use transfer::{ExportReport, ImportOptions, ImportReport, TransferContext};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod publication_tests;
