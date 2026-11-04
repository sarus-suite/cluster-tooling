//! Presence is an identity observation, independent of image health.
use crate::{
    Error, ImageSelector, ListDiagnostic, LocalRef, Result, Store, discovery, fsutil,
    locks::Operation, oci, refs,
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

        let mut discovered = discovery::discover(&guard)?;
        fsutil::checkpoint("exists.discovered", self.layout.root())?;
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
            let root =
                refs::raw_image_path(&guard, &reference).and_then(|path| oci::index_root(&path));
            match root {
                Ok(root) if &root.digest == digest => {
                    fsutil::checkpoint("exists.selected", &item.path)?;
                    return Ok(true);
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
            Ok(false)
        }
    }
}
