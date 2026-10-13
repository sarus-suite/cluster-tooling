use crate::{Error, LocalRef, Result, Sha256Digest};
use serde::{Deserialize, Serialize};

/// Store format marker. Read using `from_json` to check its version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoreMetadata {
    version: u64,
}

impl Default for StoreMetadata {
    fn default() -> Self {
        Self { version: 1 }
    }
}
impl StoreMetadata {
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        check_version(bytes, "store")?;
        Ok(Self::default())
    }
}

/// Stage-1 envelope, not yet the metadata schema for published images.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RefMetadata {
    version: u64,
    pub local_ref: LocalRef,
    pub stored_root: Sha256Digest,
}

impl RefMetadata {
    pub fn new(local_ref: LocalRef, stored_root: Sha256Digest) -> Self {
        Self {
            version: 1,
            local_ref,
            stored_root,
        }
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        check_version(bytes, "ref")?;
        #[derive(Deserialize)]
        struct Fields {
            local_ref: LocalRef,
            stored_root: Sha256Digest,
        }
        let fields: Fields = serde_json::from_slice(bytes).map_err(|source| Error::Metadata {
            kind: "ref",
            source,
        })?;
        Ok(Self::new(fields.local_ref, fields.stored_root))
    }
}

fn check_version(bytes: &[u8], kind: &'static str) -> Result<()> {
    #[derive(Deserialize)]
    struct Version {
        version: u64,
    }
    let header: Version =
        serde_json::from_slice(bytes).map_err(|source| Error::Metadata { kind, source })?;
    if header.version != 1 {
        return Err(Error::UnsupportedVersion {
            kind,
            version: header.version,
        });
    }
    Ok(())
}
