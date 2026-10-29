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
and content metadata, and includes any `cleanup_warnings`. Its
`ignored_destination_digest` field identifies a digest ignored in an explicit
import destination; it is `None` when the destination was omitted. `ExportReport`
identifies the local ref, stored manifest digest, and destination. `Store::export`
requires digest preservation for both import and pull provenance and propagates
capability and transfer errors without fallback. The CLI uses
these reports for its JSON results.

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
space. `export` returns `ExportReport`, including the deterministically chosen
local ref.

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
metadata, ignored explicit destination digest, and cleanup warnings. Pull has no
containers-storage image ID. Stored-root lookup uses the committed destination's
literal repository name, even when it differs from the registry source.

## Inspecting image availability

`Store::availability(&ImageSelector)` inspects local content in an existing,
initialized store. It uses the same selectors and structural validation as
single-image checking, including every dependency in a supported index graph.
It invokes no external tools or registry requests and never initializes a store.

```no_run
use hpc_image_store::{
    ImageAvailability, ImageSelector, PullContext, PullOptions, Store, StoreConfig,
};
# fn example() -> hpc_image_store::Result<()> {
let store = Store::new(StoreConfig::new("/scratch/user/.sarus-imagestore")?);
let image = ImageSelector::new("docker.io/library/alpine:3.23")?;

// Propagate preparation, locking, resolution, and inspection errors to the caller.
match store.availability(&image)? {
    ImageAvailability::Ready => {
        println!("Reuse the stored image {image}");
    }
    ImageAvailability::Missing => {
        store.pull(&image, None, &PullContext::default(), &PullOptions::default())?;
    }
    ImageAvailability::Unusable { diagnostics } => {
        // This caller reports the findings and leaves recovery to its operator.
        for finding in diagnostics {
            eprintln!("{:?} at {}: {}", finding.kind, finding.path.display(), finding.message);
        }
    }
}
# Ok(())
# }
```

`Ready` means the selected ref passes checking within the checker's limits.
`Missing` means the named ref or repository-qualified stored root is definitively
absent. Missing stores, permission failures, lock timeouts, and incomplete digest
searches return errors. A damaged candidate that prevents establishing a digest
match therefore cannot become `Missing`.

`Unusable` contains one or more `AvailabilityDiagnostic` values. Their `kind`
is `AvailabilityDiagnosticKind::Damage` or `Unsupported`; callers need not parse
messages to distinguish them. Each diagnostic also contains its existing
subsystem `category`, path, optional operation, logical ref and object digest,
and a contextual message. Unsupported versions and representations differ from
malformed or missing image files. Operational inspection failures return errors,
even when other damage has already been detected. Checker reports and CLI
output retain their existing behavior, and availability adds no CLI command.

The shared store and selected-ref locks remain held throughout inspection.
Results describe the content when observed and reserve nothing afterward. A
later import or pull must check its destination under its own locks, so even
acquisition after `Missing` can fail with `RefExists`. Existing refs always
require explicit replacement. Callers choose whether to report unusable content
or request replacement; reacquiring unsupported content may reproduce the same
problem, and replacing a ref cannot generally repair damaged immutable shared
CAS objects.

Availability checks no remote-tag freshness and uses no provenance aliases.
It does not hash blobs, decompress or unpack layers, or certify execution.
Same-size corruption may go undetected. Internal index traversal does not add
index acquisition or export support. Transfers do not run availability
automatically, and pruning always performs its own fresh authoritative scan.

## Library pruning

```rust,no_run
use hpc_image_store::{PruneOptions, Result, Store, StoreConfig};

fn preview_prune() -> Result<()> {
    let store = Store::new(StoreConfig::new("/scratch/me/images")?);
    let report = store.prune(&PruneOptions {
        dry_run: true,
        ..PruneOptions::default()
    })?;
    if report.has_errors() {
        for diagnostic in &report.diagnostics {
            eprintln!("{}: {}", diagnostic.path.display(), diagnostic.message);
        }
    }
    Ok(())
}
```

`Store::prune(&PruneOptions)` returns `Result<PruneReport>`. Both dry-run and
actual deletion use the same report types. Check `has_errors()` as well as the
outer `Result`: scan and deletion failures are collected, while preparation and
lock failures are returned as typed errors. The default grace is 24 hours of
object age, rather than time since removal;
unused ref bookkeeping is eligible immediately. Unknown contents are preserved,
and any incomplete authoritative scan blocks deletion. Candidate byte counts
are logical file lengths, not physical space savings. Inspect diagnostics,
partial removals, and `durability_confirmed` before interpreting a report.

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
and identity fields. Store format 1 and ref metadata schema 3 have no migration
fallback. Acquisition provenance is descriptive and never a lookup alias.

## Errors and visible changes

Store preparation, locking, selection, capability, and tool failures return
typed `Error` values with context. List, check, and prune can instead return
collected reports containing failures; check their `has_errors()` methods.
Availability's `Unusable` is a classified observation rather than an operational
error, and may carry several diagnostics.

`Error::Durability` identifies a potentially visible blob insertion, ref
publication, or removal through `NamespaceChange` and the failed sync path.
`Error::InitializationDurability` similarly reports a potentially published
marker. Do not assume `Err` means no change occurred or retry replacement
blindly. Successful import/pull reports can contain `cleanup_warnings` after
durable publication; cleanup warnings do not undo the committed ref. Prune
reports expose partial removals and uncertain durability separately.
