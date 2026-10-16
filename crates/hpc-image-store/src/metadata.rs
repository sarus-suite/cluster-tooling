use crate::{Error, LocalRef, Result, Sha256Digest};
use serde::{Deserialize, Deserializer, Serialize};

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
        check_version(bytes, "store", 1)?;
        Ok(Self::default())
    }
}

/// A containers-storage image ID, distinct from a manifest or index digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ImageId(String);
impl ImageId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let hex = value.strip_prefix("sha256:").unwrap_or(&value);
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidTransfer(
                "expected a complete lowercase containers-storage image ID".into(),
            ));
        }
        Ok(Self(hex.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for ImageId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

/// Display caches derived from completed OCI content, not operational provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContentMetadata {
    pub platform: Platform,
    pub created: Option<String>,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Acquisition {
    Import {
        source: String,
        image_id: ImageId,
    },
    // No pull operation is exposed in stage 3. This discriminator establishes
    // the export policy; registry acquisition and index support come later.
    Pull {
        requested_reference: String,
        resolved_digest: Sha256Digest,
    },
}

/// Ref schema version 2. Version 1 development envelopes are not migrated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RefMetadata {
    version: u64,
    pub local_ref: LocalRef,
    pub stored_root: Sha256Digest,
    pub content: ContentMetadata,
    pub acquired_at_unix_seconds: u64,
    pub acquisition: Acquisition,
    pub notes: Option<String>,
}
impl RefMetadata {
    pub fn new_import(
        local_ref: LocalRef,
        stored_root: Sha256Digest,
        content: ContentMetadata,
        acquired_at_unix_seconds: u64,
        source: String,
        image_id: ImageId,
        notes: Option<String>,
    ) -> Self {
        Self {
            version: 2,
            local_ref,
            stored_root,
            content,
            acquired_at_unix_seconds,
            acquisition: Acquisition::Import { source, image_id },
            notes,
        }
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        check_version(bytes, "ref", 2)?;
        #[derive(Deserialize)]
        struct Fields {
            local_ref: LocalRef,
            stored_root: Sha256Digest,
            content: ContentMetadata,
            acquired_at_unix_seconds: u64,
            acquisition: Acquisition,
            notes: Option<String>,
        }
        let fields: Fields = serde_json::from_slice(bytes).map_err(|source| Error::Metadata {
            kind: "ref",
            source,
        })?;
        let metadata = Self {
            version: 2,
            local_ref: fields.local_ref,
            stored_root: fields.stored_root,
            content: fields.content,
            acquired_at_unix_seconds: fields.acquired_at_unix_seconds,
            acquisition: fields.acquisition,
            notes: fields.notes,
        };
        if metadata.content.platform.os.is_empty()
            || metadata.content.platform.architecture.is_empty()
        {
            return Err(Error::InvalidTransfer(
                "ref metadata has an empty platform".into(),
            ));
        }
        if let Acquisition::Pull {
            resolved_digest, ..
        } = &metadata.acquisition
            && resolved_digest != &metadata.stored_root
        {
            return Err(Error::InvalidTransfer(
                "pull provenance disagrees with the stored root".into(),
            ));
        }
        Ok(metadata)
    }

    /// Synthetic stage-2 fixtures do not model an acquisition operation.
    #[cfg(test)]
    pub(crate) fn new(local_ref: LocalRef, stored_root: Sha256Digest) -> Self {
        Self::new_import(
            local_ref,
            stored_root,
            ContentMetadata {
                platform: Platform {
                    os: "linux".into(),
                    architecture: "amd64".into(),
                    variant: None,
                },
                created: None,
                size_bytes: 0,
            },
            0,
            "synthetic-fixture".into(),
            ImageId::new("a".repeat(64)).unwrap(),
            None,
        )
    }
}

fn check_version(bytes: &[u8], kind: &'static str, expected: u64) -> Result<()> {
    #[derive(Deserialize)]
    struct Version {
        version: u64,
    }
    let header: Version =
        serde_json::from_slice(bytes).map_err(|source| Error::Metadata { kind, source })?;
    if header.version != expected {
        return Err(Error::UnsupportedVersion {
            kind,
            version: header.version,
        });
    }
    Ok(())
}
