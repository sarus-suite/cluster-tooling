use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

fn is_reserved_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Select committed content by logical name or by its actual manifest/index
/// root digest. Config digests and acquisition provenance are not lookup keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageSelector {
    LocalRef(LocalRef),
    Digest(Sha256Digest),
}

impl From<LocalRef> for ImageSelector {
    fn from(reference: LocalRef) -> Self {
        Self::LocalRef(reference)
    }
}
impl From<Sha256Digest> for ImageSelector {
    fn from(digest: Sha256Digest) -> Self {
        Self::Digest(digest)
    }
}
impl FromStr for ImageSelector {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        // Only complete hexadecimal spellings are reserved. Uppercase hex
        // stays reserved but fails canonical digest validation below.
        if is_reserved_digest(value) {
            Sha256Digest::new(value).map(Self::Digest)
        } else {
            LocalRef::new(value).map(Self::LocalRef)
        }
    }
}
impl fmt::Display for ImageSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalRef(reference) => reference.fmt(f),
            Self::Digest(digest) => digest.fmt(f),
        }
    }
}

/// A local, explicitly tagged name, without registry normalization.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LocalRef(String);

impl LocalRef {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let invalid = |reason| Error::InvalidRef {
            value: value.clone(),
            reason,
        };
        let (name, tag) = value
            .split_once(':')
            .ok_or_else(|| invalid("an explicit tag is required"))?;
        if name.is_empty() || name.len() > 255 || !name.split('/').all(valid_name_component) {
            return Err(invalid("invalid repository path or name exceeds 255 bytes"));
        }
        if tag.is_empty()
            || tag.len() > 128
            || !tag.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphanumeric() || b == b'_' || (i > 0 && matches!(b, b'.' | b'-'))
            })
        {
            return Err(invalid("invalid tag or tag exceeds 128 bytes"));
        }
        if is_reserved_digest(&value) {
            return Err(invalid("this spelling is reserved for digest selectors"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn name(&self) -> &str {
        self.0.split_once(':').unwrap().0
    }
    pub fn tag(&self) -> &str {
        self.0.split_once(':').unwrap().1
    }

    /// Reversible directory key; terminal namespace entries are added separately.
    pub fn encoded_path(&self) -> PathBuf {
        let hex: String = self.0.bytes().map(|b| format!("{b:02x}")).collect();
        hex.as_bytes()
            .chunks(120)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect()
    }

    pub fn from_encoded_path(path: &Path) -> Result<Self> {
        let chunks: Vec<_> = path
            .components()
            .map(|part| match part {
                Component::Normal(part) => part
                    .to_str()
                    .ok_or(Error::InvalidEncoding("non-ASCII component")),
                _ => Err(Error::InvalidEncoding("non-relative component")),
            })
            .collect::<Result<_>>()?;
        if chunks.is_empty()
            || chunks.iter().enumerate().any(|(i, chunk)| {
                chunk.is_empty()
                    || chunk.len() > 120
                    || chunk.len() % 2 != 0
                    || (i + 1 < chunks.len() && chunk.len() != 120)
                    || !chunk
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(Error::InvalidEncoding("noncanonical hexadecimal chunks"));
        }
        let hex = chunks.concat();
        // The longest valid logical reference is 255 + ':' + 128 bytes.
        if hex.len() > 768 {
            return Err(Error::InvalidEncoding("reference is too long"));
        }
        let bytes: Vec<_> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let value =
            String::from_utf8(bytes).map_err(|_| Error::InvalidEncoding("invalid UTF-8"))?;
        let reference = Self::new(value)?;
        if reference.encoded_path().as_os_str() != path.as_os_str() {
            return Err(Error::InvalidEncoding("noncanonical path"));
        }
        Ok(reference)
    }
}

fn valid_name_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let alphanumeric = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let mut i = 0;
    while i < bytes.len() && alphanumeric(bytes[i]) {
        i += 1;
    }
    if i == 0 {
        return false;
    }
    while i < bytes.len() {
        match bytes[i] {
            b'.' => i += 1,
            b'_' => {
                i += 1;
                if bytes.get(i) == Some(&b'_') {
                    i += 1;
                }
            }
            b'-' => {
                while bytes.get(i) == Some(&b'-') {
                    i += 1;
                }
            }
            _ => return false,
        }
        let start = i;
        while i < bytes.len() && alphanumeric(bytes[i]) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    true
}

/// A canonical SHA-256 content identity. This type does not hash content.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let hex = value
            .strip_prefix("sha256:")
            .ok_or_else(|| Error::InvalidDigest(value.clone()))?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidDigest(value));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn hex(&self) -> &str {
        &self.0[7..]
    }
}

impl TryFrom<String> for LocalRef {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}
impl From<LocalRef> for String {
    fn from(value: LocalRef) -> Self {
        value.0
    }
}
impl FromStr for LocalRef {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}
impl fmt::Display for LocalRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}
impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.0
    }
}
impl FromStr for Sha256Digest {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}
impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
