//! Only the small wrapper files are interpreted here, not the image graph.
use crate::{Error, Result, Sha256Digest, fsutil};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const IMAGE_INDEX: &str = "application/vnd.oci.image.index.v1+json";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Descriptor {
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub digest: Sha256Digest,
    pub size: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Descriptor {
    pub fn validate(&self, path: &Path) -> Result<()> {
        if !matches!(self.media_type.as_str(), MANIFEST | IMAGE_INDEX) {
            return Err(Error::InvalidObject {
                path: path.to_owned(),
                reason: "root must be an OCI manifest or image index".into(),
            });
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct LayoutVersion {
    #[serde(rename = "imageLayoutVersion")]
    version: String,
}

#[derive(Deserialize, Serialize)]
struct LayoutIndex {
    #[serde(rename = "schemaVersion")]
    version: u64,
    #[serde(rename = "mediaType", skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    manifests: Vec<Descriptor>,
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fsutil::read_regular(path)?).map_err(|source| Error::Metadata {
        kind: "OCI layout",
        source,
    })
}

pub(crate) fn roots(layout: &Path) -> Result<Vec<Descriptor>> {
    let version: LayoutVersion = parse(&layout.join("oci-layout"))?;
    if version.version != "1.0.0" {
        return Err(Error::InvalidObject {
            path: layout.join("oci-layout"),
            reason: "unsupported OCI layout version".into(),
        });
    }
    let index_path = layout.join("index.json");
    let index: LayoutIndex = parse(&index_path)?;
    if index.version != 2
        || index
            .media_type
            .as_deref()
            .is_some_and(|media_type| media_type != IMAGE_INDEX)
    {
        return Err(Error::InvalidObject {
            path: index_path,
            reason: "unsupported layout index schema or media type".into(),
        });
    }
    Ok(index.manifests)
}

pub(crate) fn selected_root(layout: &Path, digest: &Sha256Digest) -> Result<Descriptor> {
    let mut matches = roots(layout)?
        .into_iter()
        .filter(|root| &root.digest == digest);
    let root = matches.next().ok_or_else(|| Error::InvalidObject {
        path: layout.join("index.json"),
        reason: "selected root is absent from the layout index".into(),
    })?;
    if matches.next().is_some() {
        return Err(Error::InvalidObject {
            path: layout.join("index.json"),
            reason: "selected root is ambiguous".into(),
        });
    }
    root.validate(&layout.join("index.json"))?;
    Ok(root)
}

pub(crate) fn single_root(layout: &Path) -> Result<Descriptor> {
    let mut roots = roots(layout)?;
    if roots.len() != 1 {
        return Err(Error::InvalidObject {
            path: layout.join("index.json"),
            reason: "committed layout must contain exactly one root descriptor".into(),
        });
    }
    let root = roots.remove(0);
    root.validate(&layout.join("index.json"))?;
    Ok(root)
}

pub(crate) fn write_layout(directory: &Path, root: &Descriptor) -> Result<()> {
    let version = LayoutVersion {
        version: "1.0.0".into(),
    };
    let index = LayoutIndex {
        version: 2,
        media_type: Some(IMAGE_INDEX.into()),
        manifests: vec![root.clone()],
    };
    for (name, bytes) in [
        ("oci-layout", serde_json::to_vec(&version)),
        ("index.json", serde_json::to_vec(&index)),
    ] {
        let bytes = bytes.map_err(|source| Error::Metadata {
            kind: "OCI layout",
            source,
        })?;
        fsutil::write_new(&directory.join(name), &bytes)?;
    }
    Ok(())
}
