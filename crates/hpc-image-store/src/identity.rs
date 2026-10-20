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

/// A literal repository name with a tag, a digest, or both. A digest selects
/// only committed roots in this repository; provenance is never a lookup key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageSelector {
    spelling: String,
    reference_len: usize,
    name_len: usize,
    digest: Option<Sha256Digest>,
}

impl ImageSelector {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let spelling = value.into();
        let (reference, digest) = match spelling.split_once('@') {
            Some((reference, digest)) => (reference, Some(Sha256Digest::new(digest)?)),
            None => (spelling.as_str(), None),
        };
        let (name, tag) = parse_reference(reference).map_err(|reason| Error::InvalidSelector {
            value: spelling.clone(),
            reason,
        })?;
        if tag.is_none() && digest.is_none() {
            return Err(Error::InvalidSelector {
                value: spelling,
                reason: "a repository name and at least one of tag or digest are required",
            });
        }
        Ok(Self {
            reference_len: reference.len(),
            name_len: name.len(),
            spelling,
            digest,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.spelling
    }
    /// The supplied `name[:tag]`, excluding the digest suffix.
    pub fn reference(&self) -> &str {
        &self.spelling[..self.reference_len]
    }
    pub fn name(&self) -> &str {
        &self.spelling[..self.name_len]
    }
    pub fn tag(&self) -> Option<&str> {
        (self.name_len < self.reference_len)
            .then(|| &self.spelling[self.name_len + 1..self.reference_len])
    }
    pub fn digest(&self) -> Option<&Sha256Digest> {
        self.digest.as_ref()
    }
}

impl From<LocalRef> for ImageSelector {
    fn from(reference: LocalRef) -> Self {
        Self {
            name_len: reference.name().len(),
            reference_len: reference.as_str().len(),
            spelling: reference.0,
            digest: None,
        }
    }
}
impl FromStr for ImageSelector {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}
impl fmt::Display for ImageSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
        let (_, tag) = parse_reference(&value).map_err(invalid)?;
        if tag.is_none() {
            return Err(invalid("an explicit tag is required"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn name(&self) -> &str {
        self.0.rsplit_once(':').unwrap().0
    }
    pub fn tag(&self) -> &str {
        self.0.rsplit_once(':').unwrap().1
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

/// Split a tag only after the last slash, so a registry port stays in the name.
/// This follows containers/image's named-reference grammar without expansion.
fn parse_reference(value: &str) -> std::result::Result<(&str, Option<&str>), &'static str> {
    if is_reserved_digest(value) {
        return Err("bare digests are unsupported; qualify the digest with a repository name");
    }
    let (name, tag) = match value.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name, Some(tag)),
        _ => (value, None),
    };
    if name.is_empty() || name.len() > 255 || !valid_name(name) {
        return Err("invalid repository path or name exceeds 255 bytes");
    }
    if let Some(tag) = tag
        && (tag.is_empty()
            || tag.len() > 128
            || !tag.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphanumeric() || b == b'_' || (i > 0 && matches!(b, b'.' | b'-'))
            }))
    {
        return Err("invalid tag or tag exceeds 128 bytes");
    }
    Ok((name, tag))
}

fn valid_name(name: &str) -> bool {
    let mut components = name.split('/');
    let first = components.next().unwrap_or_default();
    // The optional domain may contain uppercase letters and a numeric port.
    // A plain first path component may instead contain repository separators.
    let valid_first = valid_name_component(first) || (name.contains('/') && valid_domain(first));
    valid_first && components.all(valid_name_component)
}

fn valid_domain(domain: &str) -> bool {
    let host = match domain.split_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            host
        }
        None => domain,
    };
    host.split('.').all(|part| {
        let bytes = part.as_bytes();
        bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    })
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
