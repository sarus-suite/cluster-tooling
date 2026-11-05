//! Presence and digest resolution observe identity independently of image health.
use crate::{
    Error, ImageSelector, ListDiagnostic, LocalRef, Result, Sha256Digest, Store, discovery, fsutil,
    locks::{Operation, StoreGuard},
    oci, refs,
};

impl Store {
    /// Observe a name or repository-qualified stored root in an initialized store.
    /// A terminal name entry counts even if its pointer is dangling or malformed.
    /// Digest lookup reads authoritative indexes through confined pointers, without
    /// inspecting cached metadata or image content. Incomplete resolution is an
    /// error. Pending recorded recovery runs first and may change the store.
    /// This invokes no tools and reserves nothing after returning.
    pub fn exists(&self, selector: &ImageSelector) -> Result<bool> {
        let guard = self.prepare(Operation::Exists)?;
        let Some(digest) = selector.digest() else {
            let reference = LocalRef::new(selector.reference())?;
            let present = refs::namespace_entry_at(&self.layout, &reference)?.is_some();
            fsutil::checkpoint("exists.selected", &self.layout.ref_entry(&reference))?;
            return Ok(present);
        };

        Ok(find_digest(&guard, selector, digest)?.is_some())
    }

    /// Resolve a stored image to its literal repository name and root digest,
    /// formatted as `name@sha256:…` without a tag. Definite absence returns `None`.
    /// Unlike tagged existence, this requires a canonical confined pointer and
    /// a readable authoritative root index. A supplied digest overrides the tag.
    /// This requires an initialized store and completes pending recorded mutation
    /// recovery first. It reads no cached metadata or image content, invokes no
    /// tools, and certifies neither image health nor continued availability.
    /// The returned reference reserves nothing after the shared guard is released.
    pub fn resolve(&self, selector: &ImageSelector) -> Result<Option<String>> {
        let guard = self.prepare(Operation::Resolve)?;
        let digest = if let Some(digest) = selector.digest() {
            find_digest(&guard, selector, digest)?
        } else {
            let reference = LocalRef::new(selector.reference())?;
            if refs::namespace_entry_at(&self.layout, &reference)?.is_none() {
                return Ok(None);
            }
            fsutil::checkpoint("resolve.selected", &self.layout.ref_entry(&reference))?;
            let path = refs::raw_image_path(&guard, &reference)?;
            Some(oci::index_root(&path)?.digest)
        };
        Ok(digest.map(|digest| format!("{}@{digest}", selector.name())))
    }
}

/// Both queries use the same lightweight identity search under the caller's guard.
fn find_digest(
    guard: &StoreGuard<'_>,
    selector: &ImageSelector,
    digest: &Sha256Digest,
) -> Result<Option<Sha256Digest>> {
    let mut discovered = discovery::discover(guard)?;
    fsutil::checkpoint("exists.discovered", guard.layout().root())?;
    discovered
        .entries
        .sort_by(|a, b| a.local_ref.cmp(&b.local_ref).then(a.path.cmp(&b.path)));
    let mut diagnostics = Vec::new();
    for item in discovered.entries {
        if item
            .local_ref
            .as_ref()
            .is_some_and(|reference| reference.name() != selector.name())
        {
            continue;
        }
        if let Some(error) = item.diagnostic {
            diagnostics.push(ListDiagnostic::from_error(
                &item.path,
                item.local_ref.as_ref(),
                &error,
            ));
            continue;
        }
        let Some(reference) = item.local_ref else {
            continue;
        };
        let root = refs::raw_image_path(guard, &reference).and_then(|path| oci::index_root(&path));
        match root {
            Ok(root) if &root.digest == digest => {
                fsutil::checkpoint("exists.selected", &item.path)?;
                return Ok(Some(root.digest));
            }
            Ok(_) => (),
            Err(error) => diagnostics.push(ListDiagnostic::from_error(
                &item.path,
                Some(&reference),
                &error,
            )),
        }
    }
    if !discovered.complete || !diagnostics.is_empty() {
        Err(Error::ResolutionIncomplete {
            selector: selector.clone(),
            diagnostics,
        })
    } else {
        Ok(None)
    }
}
