use crate::{
    DiagnosticCategory, Error, ImageSelector, LocalRef, Result, Sha256Digest, Store, fsutil,
    graph::GraphReader,
    inspection::{self, FailureKind, Finding},
    inventory,
    locks::Operation,
};
use std::path::PathBuf;

/// Local structural health when inspected, without reserving the selected ref.
#[derive(Clone, Debug)]
pub enum ImageAvailability {
    /// Wrappers and the complete supported graph passed structural validation.
    Ready,
    /// The selector is definitively absent from a valid initialized store.
    Missing,
    /// Detected damage or unsupported content; recovery remains caller policy.
    Unusable {
        diagnostics: Vec<AvailabilityDiagnostic>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvailabilityDiagnosticKind {
    Damage,
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct AvailabilityDiagnostic {
    pub kind: AvailabilityDiagnosticKind,
    pub category: DiagnosticCategory,
    pub operation: Option<&'static str>,
    pub path: PathBuf,
    pub local_ref: Option<LocalRef>,
    pub object_digest: Option<Sha256Digest>,
    pub message: String,
}

impl Store {
    /// Inspect local wrappers and the complete supported OCI graph. Missing
    /// stores and operational failures are errors. This never acquires or
    /// repairs an image, checks registry freshness, hashes blobs, or unpacks them.
    pub fn availability(&self, selector: &ImageSelector) -> Result<ImageAvailability> {
        let guard = self.prepare(Operation::Availability)?;
        let ref_guard = match inventory::select(&guard, selector) {
            Ok(selected) => selected,
            Err(Error::DigestMissing(_)) => return Ok(ImageAvailability::Missing),
            Err(error) => return Err(error),
        };
        let reference = ref_guard.reference();
        let path = self.layout.ref_entry(reference);
        let mut reader = GraphReader::new(&self.layout.blobs());
        let inspected = inspection::inspect(&ref_guard, &mut reader, || {
            fsutil::checkpoint("availability.selected", &path)
        })?;
        if inspected.missing {
            return Ok(ImageAvailability::Missing);
        }
        let mut findings: Vec<_> = inspected
            .wrapper_findings
            .into_iter()
            .map(|finding| (finding, None))
            .collect();
        if let Some(graph) = inspected.graph {
            findings.extend(
                graph
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| (diagnostic.finding, Some(diagnostic.digest))),
            );
        }
        drop(reader);
        // Guards remain live through classification; neither selection nor
        // validation provides a reservation once this call returns.
        outcome(findings, reference)
    }
}

fn outcome(
    findings: Vec<(Finding, Option<Sha256Digest>)>,
    reference: &LocalRef,
) -> Result<ImageAvailability> {
    if let Some(index) = findings
        .iter()
        .position(|(finding, _)| finding.kind == FailureKind::Operational)
    {
        let (finding, _) = findings.into_iter().nth(index).unwrap();
        return Err(finding.into_error());
    }
    let diagnostics: Vec<_> = findings
        .into_iter()
        .map(|(finding, object_digest)| AvailabilityDiagnostic {
            kind: match finding.kind {
                FailureKind::Damage => AvailabilityDiagnosticKind::Damage,
                FailureKind::Unsupported => AvailabilityDiagnosticKind::Unsupported,
                FailureKind::Operational => unreachable!("operational failures propagated above"),
            },
            category: finding.category,
            operation: finding.operation,
            path: finding.path,
            local_ref: Some(reference.clone()),
            object_digest,
            message: finding.message,
        })
        .collect();
    if diagnostics.is_empty() {
        Ok(ImageAvailability::Ready)
    } else {
        Ok(ImageAvailability::Unusable { diagnostics })
    }
}
