//! Foundations for a Linux, user-owned filesystem OCI image store.
//! Image operations and publication are not implemented yet.

#![doc = include_str!("../README.md")]

#[cfg(not(target_os = "linux"))]
compile_error!("hpc-image-store currently supports Linux only");

mod error;
mod identity;
// These internal foundations will be used by the image operations in later stages.
#[allow(dead_code)]
mod initialization;
#[allow(dead_code)]
mod layout;
#[allow(dead_code)]
mod locks;
mod metadata;
mod store;

pub use error::{Error, Result};
pub use identity::{LocalRef, Sha256Digest};
pub use metadata::{RefMetadata, StoreMetadata};
pub use store::{Store, StoreConfig};

#[cfg(test)]
mod tests;
