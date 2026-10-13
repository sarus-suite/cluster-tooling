//! Foundations for a Linux, user-owned filesystem OCI image store.
//! Internal publication primitives are available; image operations are not yet implemented.

#![doc = include_str!("../README.md")]

#[cfg(not(target_os = "linux"))]
compile_error!("hpc-image-store currently supports Linux only");

mod error;
mod identity;
// Publication remains internal until the acquisition operations are implemented.
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
// These internal foundations will be used by the image operations in later stages.
#[allow(dead_code)]
mod initialization;
#[allow(dead_code)]
mod layout;
#[allow(dead_code)]
mod locks;
mod metadata;
mod store;

pub use error::{Error, NamespaceChange, Result};
pub use identity::{LocalRef, Sha256Digest};
pub use metadata::{RefMetadata, StoreMetadata};
pub use store::{Store, StoreConfig};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod publication_tests;
