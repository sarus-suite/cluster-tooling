# HPC image store

This crate stores container images in a user-owned directory on Linux, separate
from Podman's local image storage. It uses the Open Container Initiative (OCI)
image format and shares identical image content between stored images. You can
import an image from local Podman storage, pull one directly from a registry,
and later export it to local storage using Skopeo to copy the image data.

The crate provides a synchronous Rust library and the `hpc-image-store` CLI.
The library is private to this workspace and is not published as a package.
Stages 1–5 provide import, export by name or repository-qualified stored-root
digest, inventory, read-only graph checking, and explicit-name removal.
Inventory formatting can match Podman's image table or show richer fields and
JSON diagnostics. Stage 6.1 adds registry pull of one OCI image manifest with
its original manifest digest. Prune remains future work, and there is no `init`
CLI action. Recorded validation results are listed below.

Multi-platform stored images, complete-index storage, index-parent metadata
and inventory, platform-selecting index export, and nested-index source
resolution are unsupported and deferred beyond that PoC. A tag pull may use a
registry index to select one image manifest, without retaining the index or
other platforms. See [Registry acquisition](#registry-acquisition).

The [filesystem image store design](docs/hpc-image-store-design.md) describes
the agreed behavior and guarantees. The
[staged implementation plan](docs/hpc-image-store-implementation-plan.md)
describes the implementation order and acceptance criteria. These remain the
primary references for the design and planned work.

## Using the CLI

### Tools and storage context

Import requires Podman and Skopeo; export uses only Skopeo and can run when
Podman is unavailable. Both operations use the caller's existing **local**
containers-storage context, which is the image storage used by Podman. This
includes the current storage driver, normal `storage.conf` discovery,
environment, and user namespace. Remote Podman contexts are outside this
contract.

The library does not supply a storage specifier or read, copy, change, or
reconstruct storage configuration. It also does not select another driver,
because changing the driver changes which images are visible. There are no CLI
flags for overriding storage roots, driver settings, or the environment.

Run the CLI or a program that uses the library directly in your normal Podman context.
If your tools or storage driver report that an existing rootless user namespace is required,
`podman unshare` is a fallback using that same context:

```sh
podman unshare hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
```

The library does not launch `podman unshare` itself. Before a transfer, it checks
that Skopeo supports the required shared-blob option, platform flags when a
platform is selected, and digest preservation when required by image metadata.

### Import and export

An import copies a local image into the filesystem store and gives it a local
reference, or **ref**, such as `analysis/tool:release`. A ref is the name and tag
used to select an image in this store. An export copies the selected image back
to containers-storage under the destination name.

The store's immediate parent directory must already exist and have been safely
prepared by the user or site, as explained under [Store initialization](#store-initialization).
Import can initialize a missing or empty store; export requires an existing
store.

```sh
hpc-image-store --store /scratch/me/images import analysis/tool:release
hpc-image-store --store /scratch/me/images export analysis/tool:release
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release --replace --notes 'updated build'
hpc-image-store --store /scratch/me/images --json export analysis/tool:release localhost/restored:latest
hpc-image-store --store /scratch/me/images --platform linux/arm64/v8 import localhost/tool:latest analysis/tool:arm64
```

Import and export use `name[:tag][@digest]` selectors. A repository name and
at least one of tag or digest are required; names with registry ports work too.
Bare digests, image IDs, untagged names without digests, transport URLs, and
embedded storage settings are rejected before accessing the store. Names such
as `oci:latest` remain ordinary repository/tag pairs. No `latest` tag or registry
prefix is inferred.

For import, an omitted destination uses the source selector. Keep a tag in that
effective destination; otherwise generate a tag from the first 12 hexadecimal
characters of the explicit source digest. An explicit tagless destination is
invalid when the source has no digest. Digests in explicit import destinations
are ignored and reported, including when the operation succeeds. They neither
supply generated tags nor assert the stored root. Generated tags can collide;
collisions fail unless replacement is explicit, and no alternative tag is chosen.

For example, importing `alpine@sha256:<D>` without a destination commits
`alpine:<first12(D)>`. A destination `renamed@sha256:<E>` instead commits
`renamed:<first12(D)>`, while `renamed:release@sha256:<E>` commits
`renamed:release`; both explicit destination digests are ignored. Importing
`alpine:latest` into `renamed@sha256:<E>` fails because no source digest supplies
a generated tag. Here `D` and `E` stand for full lowercase hexadecimal digests.
If import converts source root `D` into stored root `R`, subsequent lookup uses
`alpine@sha256:<R>`; the generated tag still comes from `D`.

Export defaults to the supplied source selector verbatim, including its tag and
digest, rather than acquisition metadata or the ref chosen during lookup.
Export destination digests are passed through to Skopeo for enforcement, and a
digest mismatch fails the export.

When exporting an image by digest or under a different tag, if an image with the
same OCI image ID and content already exists in the destination containers-storage
store, `podman images` continues to show the existing values. Use
`podman image inspect` to see the added references in `RepoTags`, `RepoDigests`,
and `NamesHistory`. The image is also runnable using the repository-qualified
digest supplied as the export destination, such as
`podman run repository@sha256:<digest>`.

Import fails if the destination ref already exists, including a damaged ref,
unless `--replace` is supplied. Use `--notes` to attach optional notes to the
import. `--skopeo PATH` and `--podman PATH` select the tool executables.
`--platform` accepts `OS/ARCH[/VARIANT]`; without it, Skopeo uses its host
selection. Selecting a platform does not convert the image's architecture.

A persistent store path containing `:` receives an explicit error because
Skopeo's OCI transport cannot address that path.

Exit status 0 means success, 1 means an operation failed, and 2 means invalid
usage. With `--json`, results go to stdout; errors and warnings go to stderr.
Output from the tools is captured separately so that it does not corrupt JSON
results. A successful import can report cleanup warnings while still returning
status 0; see [Cleanup after import](#cleanup-after-import).

### Inventory, formatting, and removal

```sh
hpc-image-store --store /scratch/me/images list
hpc-image-store --store /scratch/me/images list --digests
hpc-image-store --store /scratch/me/images list --format full
hpc-image-store --store /scratch/me/images list --format json
hpc-image-store --store /scratch/me/images remove analysis/tool:release
```

The default `podman` format uses `REPOSITORY`, `TAG`, `IMAGE ID`, `CREATED`, and
`SIZE` columns. `--digests` inserts `DIGEST` before `IMAGE ID`, as in
`podman images --digests`. Image IDs are the first 12 hex characters of the
stored image's config digest, not a raw-image directory ID or an acquisition
source ID. The digest column contains the full stored manifest root digest.
Creation ages use the cached image creation time; missing or unusable values
display as `<none>`. Rows are ordered newest first, with name/path tie-breaking.
Size uses decimal units and three significant digits. Its value is stored
content size, which can differ from the size shown by Podman.

`--format full` adds full config/root digests, acquisition and creation
timestamps, platform, exact cached bytes, status, and namespace paths. JSON
contains all entries, structured diagnostics and enumeration completion.
Global `--json` is an alias for JSON output. It conflicts with explicit
`--format podman` or `full`; `--digests` requires Podman formatting. Invalid
combinations fail as usage errors before accessing the store.

List reads only the committed namespace and small wrapper files, never CAS
objects or external tools. It validates pointer confinement, layout and metadata
versions, root selection, and cached name/root agreement. A usable entry is
`unverified`: its config digest, platform, timestamps, size and image content
have not been checked against the CAS. Damaged entries stay visible, using
their namespace path if a name cannot be decoded. Diagnostics go to stderr
and are also included in JSON. Collected results are printed before returning
status 1 for damage or incomplete enumeration; empty healthy inventory succeeds.
Individual refs are read coherently under locks, without a store-wide snapshot.
Non-UTF-8 paths use a quoted, byte-escaped representation in report strings;
Rust reports retain the native `PathBuf` values.

Digest selection scans committed OCI layout roots in local-name order, limited
to the selector's literal repository name. A supplied tag is ignored when a
digest is present: `alpine:old@sha256:<R>` can select `alpine:new`, but never
`docker.io/library/alpine:new`. A healthy matching ref remains usable despite
other damage. Without a match, damage in that repository or unreadable namespace
areas whose repository is unknown produce incomplete resolution. Known damage
in other repositories does not hide a missing root in the requested repository.
Config digests and source provenance never act as lookup aliases. Unreferenced
CAS content is not a retained digest-only image.

Remove requires an explicit local `name:tag`, including for damaged refs, and
rejects every digest-bearing selector, including `name:tag@digest`. It
unlinks that committed name without following its target, saves the namespace
change, and leaves the retired raw image, shared content and lock files for
future maintenance. Directories, special files and malformed namespace parents
are preserved with an error. Removal initializes a missing store beneath a
prepared parent under the existing mutation contract, then reports the ref
missing; list, export, and check never initialize stores.

### Checking stored images

```sh
hpc-image-store --store /scratch/me/images check analysis/tool:release
hpc-image-store --store /scratch/me/images check 'analysis/tool@sha256:<stored-root>'
hpc-image-store --store /scratch/me/images --json check --all
```

Replace `<stored-root>` with the full 64 lowercase hexadecimal digest characters.
Supply either one selector or `--all`. Digest selection uses the same literal
repository-scoped lookup as export, with a digest taking precedence over a tag.
The selected ref lock remains held while its content is checked. A missing
named ref is reported as a failed entry; failed digest resolution returns an
operation error, including existing incomplete-resolution diagnostics.

Check validates the pointer, wrapper files, and current metadata envelope, then
walks the authoritative image root and all supported dependencies. It reads
every reachable manifest, nested index, and image configuration, covering all
platforms regardless of `--platform`. It checks descriptor structure, media
types, regular-file type, existence, and byte sizes. A damaged metadata file
does not prevent traversal when the authoritative root remains readable.
Checking uses no external tools and does not require registry access.

Supported content is schema-version-2 OCI image manifests and indexes, OCI
image-config JSON, and ordinary or nondistributable OCI tar layers, including
gzip and zstd representations. Configuration JSON must specify nonempty OS and
architecture, with correctly typed optional platform and creation fields.
Layers must exist locally even when their descriptors include URLs or embedded
data. Docker-specific media types, unknown schemas and media types, artifacts,
and `subject`-bearing content are reported as unresolved. Stage 5 can inspect
index graphs through internal fixtures. This capability and its tests remain
in place, but public index acquisition and export, and metadata that describes
an index parent, are deferred beyond the current PoC.

`CheckReport` contains `entries`, structured `diagnostics`, and `complete`.
Each entry includes its path, optional local ref and authoritative root, and
`passed` or `failed` status. Diagnostics include a category, path, optional ref
and object digest, and a contextual message. `complete` is false when namespace
enumeration or any requested ref or dependency cannot be fully inspected.
`has_errors()` also considers diagnostics and failed entries. `--all` continues
after individual failures, retains malformed namespace entries in the report,
and succeeds for an empty initialized store. Like inventory, it observes each
ref under its own lock without promising a single store-wide snapshot.

Human output lists each result and totals. JSON results stay on stdout even
when a collected report contains damage; diagnostics go to stderr. Status 0
means the scope passed, 1 means damage, incomplete checking, or an operation
failure, and 2 means invalid usage. Store preparation and selector-resolution
errors produce stderr diagnostics rather than a collected JSON report.

Checking does not repair or remove anything. It can create missing coordination
lock directories or files through the existing lock mechanism, and keeps their
identities stable. Observations and parsed objects are reused within the
operation, including failed observations, but every descriptor's declared size
and media type is checked. Cached config digests, platforms, creation times, and
stored sizes are not compared with graph-derived values. Blobs are not hashed,
decompressed, or unpacked, so same-size byte corruption and invalid compressed
or tar data may remain undetected. A passing result does not certify content
digests or replace backups.

### What transfers preserve and validate

Import first runs one `podman image inspect --format '{{.Id}}'` lookup.
For tag-only sources, Skopeo then copies that complete image ID, so moving the
source tag cannot change the selected image. If the image disappears, import
fails without retrying by name. For digest-bearing sources, both inspection and
copy use `name@digest`, omitting any supplied tag. The requested digest remains
part of Skopeo's argument, while inspection retains source image-ID reporting.
The stored metadata comes from the completed
OCI manifest and configuration, rather than from Podman inspection.

Skopeo writes only into a private temporary directory during import, including
when writing shared blobs. The current transfer operations require a single
OCI manifest as the image root and reject image indexes, which can describe
multiple images or platforms. With OCI defaults, Skopeo compresses uncompressed
layers and preserves supported existing compression. A content digest identifies
the exact bytes of an object. Import can change the manifest, layer
representation, and their digests, so it does not claim to preserve the original
registry identity. Pull instead preserves one platform-specific OCI image
manifest and records `Acquisition::Pull` provenance. Its existing export branch
requests digest preservation; stage 6.2 will qualify that containers-storage
round trip.

Export validates the small files and pointer that describe the selected ref,
then relies on Skopeo to report content or transfer failures. Neither import
nor export hashes layers or checks the entire graph of referenced image
objects. Export leaves the persistent ref unchanged, and any partial data at
the destination belongs to containers-storage. Export creates no temporary
staging directory in the filesystem store.

### Registry acquisition

Pull requires Skopeo, does not invoke Podman, and does not populate
containers-storage. It accepts the common image selector, an optional
destination, replacement, and notes. Destination naming follows import's
rules, including source-derived fallback tags and notices for ignored explicit
destination digests. A tag resolved during acquisition cannot supply a generated
destination tag.

```sh
hpc-image-store --store /scratch/user/.sarus-imagestore pull docker.io/library/alpine:3.23
hpc-image-store --store /scratch/user/.sarus-imagestore --platform linux/arm64/v8 pull registry.example/tool:release tool:release --notes 'job image'
hpc-image-store --store /scratch/user/.sarus-imagestore pull registry.example/tool:release --authfile /path/to/auth.json --cert-dir /path/to/certs --policy /path/to/policy.json
```

`PullContext` keeps registry settings separate from `TransferContext`. It holds
Skopeo's executable path, an optional platform, and optional `auth_file`,
`cert_dir`, `policy`, and `tls_verify` settings. The matching pull flags are
`--authfile`, `--cert-dir`, `--policy`, and `--tls-verify[=true|false]`. Omitted
settings use Skopeo's normal configuration and inherited environment, including
`REGISTRY_AUTH_FILE`. Inspection and copying receive the same settings. Skopeo
reads credentials itself; credentials and registry configuration are never
written into ref metadata. Pull does not configure containers-storage.

Before transferring layers, the library uses `skopeo inspect --raw` to resolve
one supported platform-specific image manifest:

- A digest source must identify the OCI manifest itself. An explicit digest
  takes precedence over any source tag. Index and manifest-list digests,
  including single-entry indexes, are rejected without selecting a child.
- A tag source is inspected once. A direct manifest's digest is calculated from
  the exact response bytes, without trimming or reserializing JSON. For an OCI
  index or Docker manifest list, the library selects one image descriptor and
  inspects that child by digest, verifying its type, digest, and descriptor
  size. It never looks up the mutable tag again.
- Malformed objects, artifact content, unsupported schemas or representations,
  and resolution requiring nested indexes fail. Stored content must use the
  supported OCI manifest, configuration, and layer media types. Reading a
  Docker manifest-list envelope does not enable Docker image representations.

Pull uses the following library platform policy. Its defaults can differ from
Skopeo's platform preferences and compatibility fallbacks.

| Input | Pull behavior |
| --- | --- |
| Index tag, no explicit platform | Select `linux/amd64` for an `x86_64` library target or `linux/arm64` for `aarch64`; other targets require an explicit platform. Do not detect CPU variants. |
| Index tag, explicit platform | Match descriptor OS and architecture, and variant when supplied. Without a variant, do not filter by variant. |
| Direct manifest, no explicit platform | Accept its actual platform without host selection. |
| Any source, explicit platform | Verify completed configuration OS, architecture, and any supplied variant; missing or mismatching required values fail. |

Index selection requires one distinct matching manifest digest. Consistent
duplicate descriptors collapse to one candidate; conflicting duplicates fail.
No match or several distinct matches fails with a platform diagnostic.
Descriptor order never breaks a tie. Repository names do not enforce one
architecture across all tags.

Skopeo copies the pinned `repository@manifest-digest` into private staging with
`--preserve-digests`, without `--all`, forced conversion, recompression, or
fallback to another digest. Unsupported or incompatible representations fail
without publication. Pull sets command retries to zero and does not retry a
failed resolution or copy. The completed layout must select exactly one OCI
manifest whose digest equals the resolved digest. The wrapper reads the small
manifest and configuration for metadata and checks descriptor sizes during
staging completion. It relies on Skopeo's transfer validation without invoking
the full graph checker or hashing layers.

Pull reuses import's locks, CAS insertion, replacement, durability, and cleanup.
Before publication, failure preserves any previous ref. After publication,
durability errors describe possibly visible changes and cleanup failures become
warnings. A surviving copying Skopeo retains the operation's store and ref locks
if its wrapper terminates.

Requested source and stored identity remain distinct. A tag resolved through
index `I` to manifest `A` records the requested selector and
`resolved_digest == stored_root == A`; index `I` is neither stored nor a lookup
alias. Metadata and stored size describe the manifest, configuration, and
unique layers. Store format 1 and ref metadata schema 3 are unchanged.

Stage 6.2 will build on the existing export branch for pull metadata and qualify
real registry-to-store-to-containers-storage digest preservation. Pulled content
must preserve its manifest digest or fail. Existing import-acquired export
behavior, source lookup, optional destinations, destination-digest enforcement,
and manifest reporting will remain. Export will reject index roots and cannot
choose another architecture from a single manifest. Persistent refs and metadata
remain unchanged; partial destination data belongs to containers-storage.

Stage 7 will reuse the checker's authoritative graph reader. Any encountered
index must be traversed completely or block destructive pruning; deferred public
index support does not permit ignoring dependencies. Stage 8 will qualify the
single-platform workflows, using index fixtures for tag selection and
digest-source rejection. Complete-index round trips and related qualification
remain deferred, and local PoC completion will not qualify Lustre or VAST.

## Using the Rust library

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

`TransferContext` contains only the executable paths and an optional platform
selection. The tools inherit the caller's existing storage context as described
in the CLI section. Use `ImportOptions` to request replacement or supply notes:

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
identifies the local ref, stored manifest digest, and destination. The CLI uses
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
local ref. Stage 4.1 removes `export_selected` and replaces the earlier string
and named-ref transfer arguments with selectors; callers must update explicitly.

### Pulling from the Rust library

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

### Names, digests, and metadata

Local refs use the upstream named-reference grammar with explicit, case-sensitive
tags. Repository paths are lowercase; registry domains may retain uppercase
letters and numeric ports. Names are limited to 255 ASCII bytes and tags to 128.
No registry expansion, case normalization, or implicit `latest` is applied.
The ambiguous bare spelling `sha256:<64 hex digits>` remains reserved and is
rejected, even with uppercase hex. Ordinary `sha256:latest` names still work.
`Sha256Digest` accepts only full lowercase SHA-256 hex values.

`StoreMetadata` and `RefMetadata` serialize to JSON, and their `from_json`
methods check the schema version. Identity values read from JSON undergo the
same validation as values passed to constructors. Store markers use version 1,
and ref metadata uses version 3. Older development metadata is rejected without
migration or modification. Recreate those development refs explicitly.

Ref metadata records `local_ref` and `stored_root`. Its `content` fields cache
information for display: the required config digest (OCI ImageID), platform,
optional OCI creation time, and unique
content size. The size is the sum of unique manifest, configuration, and layer
objects, excluding the small wrapper metadata files. It does not estimate how
much space removing an image would reclaim, since other images may share its
objects.

The remaining fields describe how the image was acquired:
`acquired_at_unix_seconds`, `acquisition`, and optional `notes`. An import
records the requested source and a separate containers-storage `ImageId`.
It records no original registry digests or alternate lookup keys.

## How images are stored

A ref connects a name such as `analysis/tool:release` to an image record under
`raw_images/`. This record is called a **raw image** and contains a complete OCI
layout with matching metadata. Here, “raw” describes the record below the named
ref; it does not mean that the image is uncompressed or unpacked.

The actual image objects live in a shared **content-addressed store (CAS)**
under `blobs/sha256/`. Each object is named by the SHA-256 digest of its bytes,
so images with identical manifests, configurations, or layers can share those
objects without keeping separate copies for each ref. Once published, these
objects are immutable: their bytes must never change.

The main identifiers have different purposes:

- A **ref key** reversibly encodes a logical `name:tag` for use in a path.
- A **raw-image ID** is randomly generated for one publication of an image.
  It is unrelated to Podman's image ID or the image's content digest.
- A **content digest** identifies exact stored bytes. Two raw images can have
  the same root digest while recording different acquisitions or notes.

The directory layout is:

```text
store/
├── store.json                     # Store format marker
├── blobs/sha256/<digest>           # Shared immutable manifests, configs, layers
├── refs/<encoded-key>/ref          # Relative symlink selecting a raw image
├── raw_images/raw-<id>/            # One complete underlying image record
│   ├── oci-layout                 # OCI layout version
│   ├── index.json                 # Exactly one selected root descriptor
│   ├── metadata.json              # Content display fields, acquisition, notes
│   └── blobs/                     # Empty; objects live in the shared CAS
├── tmp/operation-<id>/             # Private work for one acquisition
│   ├── layout/                    # Skopeo's unpublished OCI layout
│   └── blobs/sha256/               # Skopeo's privately transferred objects
└── locks/
    ├── store.lock                 # Coordinates operations with future prune
    └── refs/<encoded-key>/ref.lock # Stable lock for a logical name
```

The `ref` entry is a relative symlink selecting a raw-image directory. Each raw
image has an empty `blobs/` directory to satisfy the OCI layout structure, while
export supplies Skopeo's source shared-blob option to read the actual objects
from the CAS.

### Encoding names as paths

For example, `analysis/tool:release` is encoded as
`616e616c797369732f746f6f6c3a72656c65617365`. This is hexadecimal text, not a hash
or a database-assigned ID, so the library can calculate the path directly and
recover the exact original name, including tag case. Long keys are split into
path components of at most 120 characters, with exactly 120 in each component
except the last. The fixed terminal entries `ref` and `ref.lock` prevent
collisions when one encoded name is a prefix of another.

Encoding gives all logical names one filesystem-safe representation, keeping
slashes and punctuation within the encoded name and avoiding reserved layout
filenames. The resulting paths are less readable than
`refs/analysis/tool/release/`. Encoding is separate from the choice to publish
images by switching a pointer; a readable nested namespace could also point to
raw images.

### Temporary space and layout compatibility

The `tmp/` directory keeps Skopeo's writes private until an image is published.
New objects are added to the CAS using hard links, so the staging path and CAS
path then refer to the same file. If an object already exists in the CAS, the
new staged copy remains a separate file. Removing successful staging removes
these extra links and duplicate copies without deleting published content.
Staging still needs enough space for an entire acquisition before publication,
even when the final stored content is shared with other images.

This prototype layout has no migration or compatibility fallback. Recreate
outdated developer stores and fixtures explicitly; the library does not remove
them for you. Store metadata remains version 1 and ref metadata uses version 3.
There is no offline qualification machinery.

## Store initialization

### Preparing the location

An operation that changes the store can initialize a missing store directory
or an existing empty one. Its immediate parent must already exist and have
been safely prepared by the user or site. That preparation includes syncing
the parent's creation and the directory entries needed to reach it so that
they survive a crash. The library assumes this has been done; it creates no
missing ancestors and does not inspect or repair them. Stores inside other
stores are unsupported as a setup restriction, but initialization does not
search ancestors to detect nesting.

Initializing a store without a marker requires opening and syncing the
immediate parent, even when the store root already exists and is empty.
Creating a missing root also requires write permission in that parent.
A missing parent causes an error without creating a directory chain. A parent
that allows traversal but cannot be opened for syncing cannot be used to
initialize a store without a marker.

Directories created by the library use mode 0700 and files use mode 0600,
subject to the caller's umask. Skopeo chooses the permissions of the blobs it
creates, which are inside private staging and CAS directories. Existing
permissions are preserved. Reserved setup directories must be actual
directories, and markers and lock files must be regular files; symlinks in
these entries are rejected. Committed ref pointers are intentionally symlinks.
Coordination assumes cooperating processes in user-owned storage.

### Initialization sequence and retries

Initialization checks existing contents before adding setup files. It creates
only the root and minimum lock structure, takes an exclusive lock on the stable
`locks/store.lock` file, then checks the contents again. Concurrent processes
reuse the same root and lock file rather than replacing them. Initialization
can reuse recognized empty setup directories and the regular temporary marker
`.store-init.json`. It rejects populated roots without a store version,
obsolete bootstrap artifacts, unsupported versions, and incompatible reserved
entries without deleting them.

Syncing asks the filesystem to save a file's data or a directory's entries so
that they survive a crash. For a store without a marker, initialization writes
the temporary marker and syncs these items in order:

1. The temporary marker file.
2. The lock file.
3. Internal directories, working from children to parents.
4. The store root.
5. Its immediate parent.

It then atomically renames the temporary marker to `store.json` and syncs the
root before returning success. The parent is always synced before the marker
is published, including on retries and when another process created the root.
No directory above the immediate parent is opened for syncing. A failure before
marker publication leaves recognized setup entries available for retry.

Completed initialization leaves only `store.json`, the stable
`locks/store.lock`, and empty internal directories. Ref entries, raw images,
and operation directories appear later as images are acquired and published.
A supported version-1 store needs no private recovery record.

### Preparing an existing store

For a store that already has a marker, preparation for a change takes a shared
store lock, validates the store, and syncs the marker and root again without
opening the parent. A visible marker does not allow these syncs to be skipped;
repeated sync failures remain errors until syncing succeeds.

A marker or root sync error after marker publication is reported as
`Error::InitializationDurability`. The error reports potentially published
state and leaves the marker in place. Normal operation locking and validation
follow preparation, whether the store was newly initialized or already marked.

Preparation for reading requires an existing lock and supported marker and
does not initialize or repair the store. Reading can still create a ref lock
file as coordination bookkeeping.

## Publishing images and handling failures

**Publication** makes a prepared image available through a ref. It first saves
a complete new raw image, then atomically switches the ref symlink to that
record. Readers therefore select either the complete old layout or the
complete new layout, with its matching metadata. Updating `index.json` and
`metadata.json` individually in a live directory would expose intermediate
states, and an ordinary directory rename cannot replace an existing nonempty
directory.

Import and pull use the same publication primitives. Those primitives also
support internal index fixtures, but image-index acquisition and export remain
deferred beyond the PoC.

### Preparing and saving the image

Acquisition staging provides private layout and shared-blob paths under
`tmp/operation-<id>/`. Before completing a stage, the producer must stop all
writers and close writable handles. Completing the stage gives up writable
acquisition access and binds the selected descriptor, metadata, and object list
to the locked ref. A descriptor identifies a referenced OCI object; the
acquisition layer supplies validated SHA-256 identities and expected sizes.
The publication primitives do not hash the objects or validate the complete
image graph.

Publication syncs the staged objects, then inserts them into
`blobs/sha256/<digest>` using hard links on the same filesystem. Existing digest
paths are never overwritten. To reuse one, the library requires a regular file
of the expected size and syncs the file and directory, preserving the file's
modification time. These checks cannot detect corruption that leaves the file
at the same size. If hard links are unsupported, publication fails without
falling back to copying. Published objects must never be modified through
staging paths that still refer to the same files.

The library exclusively creates each raw image with small OCI wrapper files,
matching versioned metadata, and an empty real `blobs/` directory. It normalizes
`index.json` to exactly one selected root descriptor while retaining the
descriptor's extra fields. The internal primitives allow either an OCI manifest
or image index as the root, although import, pull, and export accept only
manifests. Content blobs remain unchanged, including the bytes and
identity of an image-index root.

Publication writes and syncs the raw image files, then syncs its `blobs/`
directory, the raw-image directory, and the `raw_images/` parent. It creates
and syncs the parent directories for the ref and prepares a relative symlink
in private staging. After syncing the staging directory, a single rename
switches the committed ref to the complete raw image. Both rename parents are
then synced before success. The store lock and matching exclusive ref lock
remain held throughout.

### Replacement, removal, and durability errors

An existing destination, including a dangling symlink, causes an error unless
replacement is explicitly requested. Replacement accepts symlinks and regular
files but rejects directory entries. Replaced raw images remain for future
prune; an individual acquisition does not delete them.

Removal unlinks the terminal ref entry and syncs its
parent without following the pointer. It leaves shared content, retired raw
images, namespace directories, and lock files in place.

Before the ref is committed, failures preserve any previous ref. Once a
filesystem entry has changed, a sync failure can mean that the change is
visible but might not survive a crash. `Error::Durability` reports the
potentially visible blob insertion, ref publication, or ref removal, together
with the path whose sync failed. The library never rolls back a committed
pointer after a sync failure, because doing so would introduce another change
instead of resolving whether the committed change was saved.

### Cleanup after acquisition

Once an import or pull has durably published its ref, it removes only its own operation
directory and syncs `tmp/`, while still holding the store and ref locks.
Cleanup does not follow symlinks or scan other operations. Removing these
staging files does not remove the published image.

Cleanup is best effort after successful publication. On normal success,
`ImportReport.cleanup_warnings` and `PullReport.cleanup_warnings` are empty. A removal or cleanup-sync failure
adds a structured `CleanupWarning` containing the staging path and a contextual
message, while the ref remains committed and usable. The CLI prints warnings
on stderr, includes them in JSON results, and exits with status 0. A cleanup-sync
warning means deletion may not survive a crash; a removal failure may leave
staging only partly removed.

Acquisition failures, publication failures (including durability errors), and
process termination do not trigger cleanup, because a surviving Skopeo process
may still be writing to its private staging. Abandoned staging, cleanup
remnants, unpublished or retired raw images, and unreferenced objects remain
for future conservative pruning. Any staging paths that still refer to
published blobs must never be used for writes.

### Reading refs and discovering entries

Readers retain their ref and store lock guards while resolving and using a raw
image. Pointers must use the exact relative path into the raw-image area.
Namespace parents, raw-image directories, and small files reject redirection
through symlinks.

Internal discovery reads namespace entries rather than hiding refs with broken
pointers or malformed terminal entries. Unexpected branches and enumeration
failures are retained as diagnostics and mark discovery incomplete. Discovery
only enumerates names; inventory validates pointers and wrapper files under
each ref's shared lock. Digest resolution retains its selected ref lock until
the transfer completes, then export performs its existing small source checks.

## How locking works

Locks belong to the store and logical ref names rather than individual raw
images. Lock files stay in place across replacement and removal, so processes
continue to coordinate through the same files even when a ref selects a new
image.

The operation lock policy is shown below. Import, pull, export, remove, list,
and check are public today; prune remains planned.

| Operation | Store lock | Ref lock |
| --- | --- | --- |
| import, pull, remove | Shared | Exclusive |
| export, list, check | Shared | Shared |
| prune | Exclusive | None |

Initialization without a marker has a separate phase with an exclusive store
lock. Preparation for a change to a marked store uses shared locking for its
repeated syncs. Either path is followed by normal operation locking and
validation. A ref guard borrows its store guard, which ensures that the store
lock outlives the ref lock.

### Waiting for locks

To acquire a lock, the library makes nonblocking `flock` attempts, retrying
every 100 ms after contention or interruptions. While waiting, it prints a
warning to stderr every 30 seconds containing the lock path, mode, elapsed
time, and timeout. If a lock cannot be acquired within five minutes, the library
returns `Error::LockTimeout`. The deadline starts immediately before
the first lock attempt; filesystem open calls and operations after the lock is
acquired are outside this limit. These timings are internal policy, and tests
use millisecond thresholds instead of waiting for production intervals.

### Locks during subprocess execution

Locks are released when their owning file descriptors close. Lock files are
never truncated, replaced, or removed, including after a timeout.

The parent's lock descriptors are marked close-on-exec, which normally prevents
them from being retained when executing another program. Immediately before
executing a Skopeo copy, the child clears this flag on only the two operation
lock descriptors. The child then shares the existing locked open-file
descriptions, so closing the parent's descriptors does not release the locks
while the child still holds them. No explicit unlock is issued.

If the wrapper terminates during the copy, it publishes no ref. A surviving
Skopeo process can finish its private copy or fail, and its locks are released
when it exits. Operations that need conflicting locks must therefore wait for
the orphaned copy to release them.

## Validation and remaining qualification

### Local checks and test coverage

Run the following commands from the workspace root. The suite needs Podman,
Skopeo, Python 3, tar, and sha256sum, plus permission to create rootless user
namespaces and use overlay storage. The existing local real-tool tests run as
part of ordinary `cargo test`; no `--ignored` flag or external Bash harness is
needed for those tests. The Docker Hub test requires explicit selection as
described below.

```sh
cargo check --locked -p hpc-image-store
cargo test --locked -p hpc-image-store
cargo clippy --locked -p hpc-image-store --all-targets -- -D warnings
cargo fmt -p hpc-image-store -- --check
git diff --check
```

Tests cover names and encoding, metadata, initialization recovery, private
permissions, and rejected stores. Initialization tests verify missing-parent
rejection, exact sync ordering, persistent sync failures, unprivileged parent
permissions, and process termination after root creation, lock creation,
scaffolding, parent sync, and marker publication.

Stage-5 fixtures cover manifests, all supported layer types, all platforms and
nested indexes, a chain of 1,500 nested indexes, cycles, conflicting descriptors,
malformed JSON and descriptors, missing content, and rejected symlinks,
directories, and FIFOs. Cache tests cover reused successes and failures, while
public checks verify that shared failures affect every dependent ref. CLI tests
exercise human and JSON results, partial failures, invalid usage, empty stores,
and non-UTF-8 namespace paths without requiring tool executables. Process
handshakes verify ref/store lock retention and replacement or removal after
discovery. Snapshot checks preserve image content, metadata, pointers, staging,
and stable lock identities. Real-tool acceptance also checks a newly imported
image through both the library and JSON CLI.

Publication tests use complete zero-layer manifest and image-index fixtures.
They exercise insertion races and replacement in separate processes,
termination around the rename boundary, and test-only failure checkpoints at
insertion and sync steps. Synthetic fixtures use version-3 ref metadata, but
their placeholder acquisition fields do not represent real imports. Fault
injection is thread-local and unavailable in production builds.

Stage-3 synthetic and CLI tests exercise source-ID pinning, inherited
environment, platform flags, clean CLI JSON, unchanged caller configuration,
failed copies, metadata, replacement, shared-content reuse, and injected
publication failures. They also verify that transport-like repository names
and unqualified names reach the tools without changing transport selection.
Cleanup tests cover successful imports and replacements, preservation of CAS
inodes, bytes, and modification times, unrelated staging, symlink confinement,
and removal or sync warnings after commit. An unprivileged CLI test exercises
real removal and cleanup-sync failures, successful exit status, and separate
stderr diagnostics in both human and JSON modes.

Coordination tests cover stable lock identities, separate processes, lock-wait
warnings, and acquisition timeouts. Process tests verify both inherited locks
after wrapper termination using readiness handshakes and bounded waits.
The six ignored tests (`lock_worker`, `wait_worker`, `publication_worker`,
`transfer_worker`, `pull_worker`, and `cleanup_warning_worker`) are internal subprocess helpers
invoked by ordinary tests. Do not run these helpers directly or use an
unfiltered `--ignored` invocation. Neither real-tool acceptance test is ignored.

### Tests with real tools

Run the containers-storage acceptance test directly with Cargo:

```sh
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
```

By default, the test caller prepares a fresh temporary overlay storage
configuration, verifies Podman's effective graph root, and launches the
acceptance test in a child process with `CONTAINERS_STORAGE_CONF` set. Podman,
Skopeo, the library, and the CLI all inherit this same context. The library does
not prepare or change storage configuration. The caller resets only its own
disposable storage after the child exits, including after test failure, then
removes its temporary files. No registry access or image download is needed.

To qualify another configuration or driver, prepare an **empty disposable**
containers-storage context yourself, then opt in explicitly:

```sh
export CONTAINERS_STORAGE_CONF=/path/to/disposable/storage.conf
export HPC_IMAGE_STORE_TEST_DISPOSABLE=1
cargo test --locked -p hpc-image-store --test real_transfers -- --nocapture
```

Both tools must select the same disposable storage. With the tested older
rootless Podman, set `rootless_storage_path` as well as `graphroot` in that
configuration. Do not select your ordinary image store: the test retags and
removes images to exercise races and failures. It rejects nonempty storage
because cached layers can mask missing-source export failures. Caller-supplied
storage is left for the caller to clean up and must be recreated or emptied
before another run.

These commands run without an outer `podman unshare`. If the deployment's tools
require an existing rootless namespace, prefix the Cargo command with
`podman unshare` as a fallback. A sandbox that prohibits user namespace setup
must also permit the real-tool subprocesses; wrapping them in `podman unshare`
does not grant that permission.

The test covers library and JSON CLI round trips, OCI compression, shared-layer
reuse, explicit platform selection, replacement, tag movement after inspection,
image removal before copying, missing-content export failure, staging cleanup,
and unchanged configuration. During a real containers-storage export, it also
terminates the CLI parent, proves that the surviving Skopeo retains both store
and ref locks, and verifies that the exported image is complete after it exits.

The independent real-Skopeo OCI-to-OCI lock test needs no Podman storage:

```sh
cargo test --locked -p hpc-image-store actual_skopeo_retains_locks_after_parent_termination -- --nocapture
```

### Docker Hub smoke test (explicit only)

Ordinary `cargo test` and CI do not contact Docker Hub. All registry edge cases
use controlled Skopeo fixtures. The following single ignored test performs one
CLI acquisition of `docker.io/library/alpine:3.23` for `linux/amd64` into a
temporary store, then verifies metadata, inventory, and checking locally:

```sh
cargo test --locked -p hpc-image-store --test registry_pull dockerhub_alpine_smoke -- --ignored --exact --nocapture
```

Run it at most once per acceptance session and never in a CI matrix. It performs
no preliminary registry probe, additional library pull, automatic retry, or
fallback to another image. Network, authentication, rate-limit, and unsupported
representation failures remain failures and leave live acquisition qualification
incomplete. Existing caller authentication may be inherited without modifying
it. The temporary store is discarded when the test exits. This test does not
qualify registry-to-containers-storage export; that belongs to stage 6.2.

The six ignored subprocess helpers, including `pull_worker`, are invoked only
by their owning tests. The Docker Hub smoke test is separately ignored for its
network access. Never use an unfiltered `--ignored` invocation.

### Recorded results and outstanding qualification

On 2026-10-21, stage 6.1 passed 138 ordinary tests, including five doctests and
both existing real-tool tests. Six internal subprocess helpers and the Docker
Hub smoke test were ignored in that run. Crate check, Clippy with warnings
denied, formatting, and diff checks passed. Real local acceptance used Podman
5.4.2 and Skopeo 1.18.0 with fresh disposable overlay storage.

The Docker Hub smoke test was explicitly run once and passed, storing
`docker.io/library/alpine:3.23` for `linux/amd64` as manifest
`sha256:1f3591b8a02ea153f41c5bba878ad477f63ab3d19349762cb77504db02a23e15`.
Its metadata, inventory, stored-root selection, and graph checks passed locally.
No registry retries or additional pulls were performed. Controlled tests cover
registry resolution, platform policy, option forwarding, CLI/library parity,
failed replacement, publication and cleanup faults, concurrent pulls, and
orphaned-copy lock retention. Store format 1 and ref metadata schema 3 are
unchanged. Stage 6.2 export qualification, pruning, and Lustre/VAST site testing
remain pending.


On 2026-10-20, stage 5 passed 120 tests, including four doctests and both
real-tool tests, with the existing five internal subprocess helpers ignored.
Crate check, Clippy with warnings denied, formatting, and diff checks passed.
The full suite used Podman 5.4.2 and Skopeo 1.18.0 with fresh disposable overlay
storage. After the full run, the final graph-fixture refinements and added
documentation example passed targeted graph tests and all four doctests.
No Lustre or VAST qualification was performed. Store format 1 and ref metadata
schema 3 remain unchanged; stage 6 is next.

On 2026-10-20, stage 4.1 passed 103 tests, including three doctests and both
real-tool tests, with five internal subprocess helpers ignored. Crate check,
Clippy with warnings denied, formatting, and diff checks passed. Cargo checks
used its configured build cache outside the sandbox, and real-tool tests used
fresh disposable overlay storage with Podman 5.4.2 and Skopeo 1.18.0.

Stage-4.1 coverage includes literal selector parsing and registry ports,
repository-scoped lookup, deterministic duplicate-root selection, damaged and
incomplete scans, lock retention during concurrent changes and orphaned copies,
source-derived tag collisions and replacement, ignored import destination
digests, and early CLI/library validation. Real acceptance verified imports by
containers-storage manifest digest, source-derived tags, qualified stored-root
exports, and matching and mismatching export destination digests. Skopeo may
reject a mismatch before copying or during its destination digest check; both
failures are propagated. The fixture empties its verified disposable image
cache before the missing-content test because digest destinations can retain
additional names. Store format 1 and ref metadata schema 3 are unchanged.

On 2026-10-18, stage 4 passed the full crate suite: 96 tests passed, including
three doctests and both real-tool tests, with five internal subprocess helpers
ignored. The rootless containers-storage test used Podman 5.4.2 and Skopeo
1.18.0 with fresh disposable storage. It verified export by root digest and
that the displayed OCI config ID matches Podman's image ID. Crate check,
Clippy with warnings denied, formatting, and diff checks passed.

New fixtures cover damaged and incomplete inventories, missing CAS content,
schema-2 rejection without modification, partial JSON results (including
non-UTF-8 paths), formatter ages and sizes, duplicate-root selection, and
explicit damaged-ref removal. Subprocess handshakes exercise replacement and
removal after enumeration, retention of the selected ref lock, and orphaned
Skopeo lock retention for digest export. These are local results; no Lustre or
VAST qualification was performed.

On 2026-10-17, the optional-destination refinement passed the complete crate
suite: 74 tests passed, including doctests and both real-tool tests, with five
subprocess helpers ignored. The real containers-storage round trip used Podman
5.4.2 and Skopeo 1.18.0 outside the sandbox. Cargo check, Clippy with warnings
denied, formatting, and diff checks passed. Focused fixtures cover omitted
destinations, invalid implicit import refs, replacement, and export defaults
that differ from the recorded acquisition source.

On 2026-10-16, local acceptance passed in the integration devcontainer with
Podman 4.3.1 and Skopeo 1.9.3 using rootless native overlay. The real
containers-storage test passed both directly and with an outer `podman unshare`;
the direct run exercised both library operations and CLI commands. An outer
`podman unshare` was unnecessary in this environment.

The complete Cargo suite passed with both real-tool tests enabled: 70 tests
passed, with only the five subprocess helpers marked ignored. Cargo check,
Clippy with warnings denied, formatting, and diff checks also passed. No
production transfer changes or Bash harnesses were needed. Cleanup failure
warnings and unexpected index rejection remain covered by controlled fixtures.
FUSE overlay remains unverified because this container has no `/dev/fuse`.

Earlier checks with Podman 4.9.5 and Skopeo 1.14.4 encountered a `newuidmap`
permission failure; those historical environment limits do not describe this
successful run. In the current sandbox, namespace setup initially failed with
`/proc/.../setgroups: permission denied`; the real containers-storage tests
passed after running outside that sandbox. The first disposable configuration
also exposed Podman's need for `rootless_storage_path`, and reusing storage
from a failed run exposed cached-layer interference. The Cargo fixture now
prepares fresh storage and checks its selection and initial emptiness.

Local tests do not qualify Lustre or VAST locking, cross-node visibility,
atomic rename, or durability guarantees. Those require deployment-site
filesystem testing, and no site filesystem support is claimed here. If a
required filesystem primitive is unavailable, the library returns an error
without substituting a weaker fallback.
