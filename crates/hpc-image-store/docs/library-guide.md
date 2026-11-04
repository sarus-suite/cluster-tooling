# Rust library guide

The synchronous library owns preparation, locking, transfers, and storage
behavior. It is private to this workspace. The examples below are included in
the generated crate documentation and checked by Cargo doctests.

## Configuration and operations

Create a store configuration and handle, then construct the names and digests
you need:

```rust
use hpc_image_store::{LocalRef, Sha256Digest, Store, StoreConfig};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let reference = LocalRef::new("analysis/tool:Release_1")?;
let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64)))?;
# Ok::<(), hpc_image_store::Error>(())
```

Constructing the configuration and handle does not change the filesystem.
A relative store path is resolved against the current working directory when
the configuration is constructed. Import, pull, and export perform their own
store preparation and locking, so callers do not need to coordinate those steps.
All operations finish pending recorded recovery during preparation. Shared
queries may temporarily require exclusive access for this prerequisite, then
retry under their normal shared lock. A clean preparation performs no content
graph traversal. Unrecorded remnants are not automatically recovered.

Import, pull, and remove record their complete mutation lifecycle. Only import
and pull may initialize a missing or empty store at this stage. Remove requires
an initialized store and never creates a missing root or resumes setup.

Checking returns collected damage as a report, while store and selection
failures return typed errors:

```no_run
use hpc_image_store::{CheckScope, ImageSelector, Store, StoreConfig};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let selected = store.check(&CheckScope::Image(
    ImageSelector::new("analysis/tool:release")?,
))?;
assert!(!selected.has_errors());
let all = store.check(&CheckScope::All)?;
for diagnostic in &all.diagnostics {
    eprintln!("{}: {}", diagnostic.path.display(), diagnostic.message);
}
# Ok::<(), hpc_image_store::Error>(())
```

`CheckReport` contains entries, structured diagnostics, and `complete`. Entry
statuses describe passing or failing structural inspection; `complete` describes
whether the requested scope could be fully inspected. Inspect `has_errors()`
even when the outer call succeeds. A collected damaged-image report and an
operational `Err` are different outcomes.

`TransferContext` contains only the executable paths and an optional platform
selection. The tools inherit the caller's local containers-storage configuration,
environment, and user namespace; the library does not reconstruct that context.
Use `ImportOptions` to request replacement or supply notes:

```no_run
use hpc_image_store::{ImageSelector, ImportOptions, Store, StoreConfig, TransferContext};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let source = ImageSelector::new("localhost/tool:latest")?;
let destination = ImageSelector::new("analysis/tool:release")?;
let context = TransferContext::default();
let imported = store.import(&source, Some(&destination), &context,
                            &ImportOptions::default())?;
let restored = ImageSelector::new("localhost/restored:latest")?;
let exported = store.export(&destination, Some(&restored), &context)?;

// None uses the source selector; a tagless import source supplies a digest tag.
let other = ImageSelector::new("analysis/other:release")?;
let imported = store.import(&other, None, &context, &ImportOptions::default())?;
let exported = store.export(&other, None, &context)?;
# Ok::<(), hpc_image_store::Error>(())
```

Import and export take `&ImageSelector` sources and `Option<&ImageSelector>`
destinations. `ImageSelector::new` and `FromStr` validate inputs; `as_str` and
`Display` preserve the spelling. Its `reference`, `name`, `tag`, and `digest`
accessors expose the components separately. `From<LocalRef>` produces a tagged
selector; unqualified digest conversion is unsupported. Invalid import naming
returns `Error::InvalidImportDestination` before store preparation or tool
invocation, and the CLI reports status 2.

`ImportReport` identifies the local ref, stored root digest, source `ImageId`,
and content metadata. Its `ignored_destination_digest` field identifies a digest
ignored in an explicit import destination; it is `None` when the destination
was omitted. `ExportReport` identifies the local ref, stored manifest digest,
and destination. `Store::export` requires digest preservation for both import
and pull provenance and propagates capability and transfer errors without fallback.
The CLI uses these reports for its JSON results.

Import replacement requires explicit `ImportOptions::replace` and an old
canonical pointer with readable authoritative root/dependency declarations.
Malformed pointers, regular-file refs, missing old wrappers, or unsupported or
unreadable declarations are preserved and rejected before publication. Damaged
cached metadata alone does not prevent reclamation. Import does not repair
external damage or reclaim unrelated unrecorded content.

Completed import and pull acquisitions validate their staged dependency
declarations with recovery's parser before publication. Unsupported objects,
including subject/artifact-bearing manifests, are rejected while private.
For both acquisitions this returns acquiring-phase `Error::Recovery` context and allows
later preparation to clean abandoned staging without changing a committed ref.

Inventory and removal need no transfer context. Formatters only turn a collected
report into a string; they do not open the store or print diagnostics:

```no_run
use hpc_image_store::{ImageSelector, Store, StoreConfig, TransferContext,
                      format_list_podman, presentation};

let store = Store::new(StoreConfig::new("/scratch/me/images")?);
let report = store.list()?;
let table = format_list_podman(&report, false);
let table_with_digests = format_list_podman(&report, true);
let full_table = presentation::format_list_full(&report);
// report.has_errors() includes damaged refs and incomplete enumeration.
let selector: ImageSelector = "analysis/tool:release".parse()?;
let destination = ImageSelector::new("localhost/restored:latest")?;
store.export(&selector, Some(&destination), &TransferContext::default())?;
if let Some(entry) = report.entries.iter().find(|entry| entry.stored_root.is_some()
                                               && entry.local_ref.is_some()) {
    let root = entry.stored_root.as_ref().unwrap();
    let name = entry.local_ref.as_ref().unwrap().name();
    let by_digest = ImageSelector::new(format!("{name}@{root}"))?;
    store.export(&by_digest, Some(&destination), &TransferContext::default())?;
}
# Ok::<(), hpc_image_store::Error>(())
```

`ListReport` contains `entries`, `diagnostics`, and `complete`. The completion
flag describes namespace enumeration, separately from damage. Store-level
failures return `Err`; per-ref failures stay in the report. `ListEntry` preserves
independently readable root and metadata fields when another wrapper file is
damaged. `RemoveReport` identifies the removed name without estimating reclaimed
space. A successful `remove(&LocalRef)` means the name's removal, unused recorded
content reclamation, namespace cleanup, and record clearing are durable. It
preserves shared objects and prefixes needed by longer names. It requires a
canonical pointer, readable wrapper index, and supported readable dependency
declarations; damaged display metadata alone does not block it. Removal rejects
damaged identities rather than acting as a repair operation. `export` returns
`ExportReport`, including the deterministically chosen local ref.

## Pulling from the Rust library

```no_run
use hpc_image_store::{ImageSelector, PullContext, PullOptions, Store, StoreConfig};
# fn example() -> hpc_image_store::Result<()> {
let store = Store::new(StoreConfig::new("/scratch/user/.sarus-imagestore")?);
let source = ImageSelector::new("docker.io/library/alpine:3.23")?;
let report = store.pull(&source, None, &PullContext::default(), &PullOptions::default())?;
assert_eq!(report.resolved_digest, report.stored_root);
# Ok(())
# }
```

`PullOptions` supplies replacement and notes. `PullReport` returns the actual
committed ref, requested selector, resolved digest, stored root, content
metadata, and ignored explicit destination digest. Pull has no
containers-storage image ID. Stored-root lookup uses the committed destination's
literal repository name, even when it differs from the registry source.

Pull success includes durable cleanup and immediate reclamation of unused
superseded content. Replacement follows import's safety policy: the old pointer
and authoritative dependencies must be readable and supported; display metadata
is not used to authorize reclamation. There is no cleanup-warning report field
or shared warning type.

## Observing image existence

`Store::exists(&ImageSelector) -> Result<bool>` observes identity in an existing,
initialized store. Use explicit `Store::check` when you need structural health information.

```no_run
use hpc_image_store::{ImageSelector, PullContext, PullOptions, Store, StoreConfig};
# fn example() -> hpc_image_store::Result<()> {
let store = Store::new(StoreConfig::new("/scratch/user/.sarus-imagestore")?);
let image = ImageSelector::new("docker.io/library/alpine:3.23")?;

if store.exists(&image)? {
    println!("The stored name {image} is present");
} else {
    store.pull(&image, None, &PullContext::default(), &PullOptions::default())?;
}
# Ok(())
# }
```

If the selector does not supply a digest, existence means the canonical encoded name has a terminal
`ref` entry. A directory used only as a longer name's prefix is insufficient.
The terminal entry is inspected without following it, so dangling and malformed
pointers count as present. Unsafe namespace parents and access failures remain
errors; explicit check diagnoses damaged image content.

If the selector supplies a digest, lookup ignores a supplied tag and compares authoritative stored
roots in the same literal repository. It follows only canonical confined
pointers and reads candidate `index.json` files. It does not read cached metadata,
`oci-layout`, the raw image's `blobs/` wrapper, or CAS objects. Config IDs,
provenance, and unreferenced blobs are not aliases. A readable match establishes
presence despite other damage. Without a match, incomplete discovery or unreadable
relevant roots return `Error::ResolutionIncomplete`, rather than false. Damage
in known refs from another repository does not obscure a definite miss.

Missing or uninitialized stores, permission failures, and lock timeouts are
errors. Preparation first completes pending recorded recovery under exclusive
access or returns an error. That recovery may change the store; lookup then
retains shared access throughout its observation. Clean-store lookup initializes
nothing, changes no persistent content, invokes no tools or remote requests,
and traverses no image graph.

Existence reserves nothing afterward and certifies neither health nor execution.
A later acquisition checks its destination under its own exclusive lock and can
still fail with `RefExists`. Existing refs require explicit replacement; replacing
a ref cannot generally repair damaged immutable shared CAS objects. Neither an
existence result nor an earlier check authorizes later reclamation; mutations
and recovery read authoritative dependency declarations when cleanup is needed.
Unrecorded remnants remain preserved without a general reclamation API.

## Registry settings and metadata types

`PullContext` contains the Skopeo executable, optional platform, and optional
`auth_file`, `cert_dir`, `policy`, and `tls_verify` settings. Unset settings use
Skopeo's normal configuration and inherited environment. Both resolution and
copying use the same settings; credentials are never stored in image metadata.
Pull never invokes Podman or populates containers-storage.

For an index tag without an explicit platform, pull defaults to `linux/amd64`
on an `x86_64` library target or `linux/arm64` on `aarch64`; other targets require
an explicit platform. Direct manifests need no host selection. Explicit OS,
architecture, and any supplied variant must match the completed configuration.
Index selection requires one distinct matching digest and never chooses by
descriptor order. Pull defaults can differ from Skopeo's import/export defaults.

`LocalRef` represents an explicit tagged namespace identity, while
`Sha256Digest` requires a complete lowercase SHA-256 digest. Removal accepts a
`LocalRef`, rather than an `ImageSelector`, so it never resolves a digest.
`StoreMetadata::from_json` and `RefMetadata::from_json` validate their versions
and identity fields. Store format 2 and ref metadata schema 3 have no migration
fallback. Acquisition provenance is descriptive and never a lookup alias.

## Errors and visible changes

Store preparation, locking, selection, capability, and tool failures return
typed `Error` values with context. List and check can instead return
collected reports containing failures; check their `has_errors()` methods.
`exists` returns false only for definite absence in a valid initialized store.
It returns typed errors when preparation or identity resolution cannot finish.

`Error::InvalidMutationRecord` identifies invalid durable bookkeeping.
`Error::Recovery` reports the record path, available ref identity, phase,
potential namespace change, remaining work, and underlying cause. Recovery
failures block the requested action, including explicit check,
and preserve pending evidence for investigation. Shared cached structural or
JSON findings return `Error::InvalidObject` with the finding's path and diagnostic;
shared operational I/O failures retain their typed cause chain. Previously
published unsupported content still blocks recovery with a contextual error,
without panicking or removing the ref, record, or content. The library does not
repair or adopt inconsistent contents. No public recovery
method or report type is added.

`Error::Durability` identifies a potentially visible blob insertion, ref
publication, or removal through `NamespaceChange` and the failed sync path.
`Error::InitializationDurability` similarly reports a potentially published
marker. Do not assume `Err` means no change occurred or retry replacement
blindly. Import, pull, and remove errors after recording begins use `Error::Recovery`, retaining
the underlying typed cause, including `Error::ToolFailed` or `Error::Durability`.
Cleanup failures are errors even after publication or removal; a later operation
attempts recorded work during preparation. No mutation cleans up in a destructor.

Removal checks remaining refs after unlinking, without an additional early
preflight. Damage in another ref can therefore leave the name absent while
cleanup remains pending. Until safe recovery is possible, all ordinary operations
are blocked, including list, explicit check, and remove. They provide no
recovery bypass. Preserve pending records and the permanent lock; structural
damage may need coherent restoration or maintainer-reviewed repair. Addressing
this access limitation is deferred to PoC v3 or later, outside PoC v2.

Pull recovery does not retry the transfer or contact the registry. Capability
and source-resolution failures before recording retain their original error
types.
