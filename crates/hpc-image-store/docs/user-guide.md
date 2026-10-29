# User guide

The `hpc-image-store` CLI operates on a persistent user-owned image store selected
with `--store PATH`. The [administration guide](administration.md) covers building,
installation, and preparing the store's immediate parent. The
[architecture](architecture.md) defines the storage and failure-handling contracts;
the [library guide](library-guide.md) covers Rust callers.

## Tools and storage context

Import requires local Podman and Skopeo. Export and registry pull require Skopeo
only; inventory, checking, removal, and pruning invoke neither tool. Import and
export inherit your local containers-storage configuration, environment, driver,
and user namespace. The library does not change that configuration or select
another driver. Remote Podman contexts are unsupported.

Run directly in your normal local context. If the tools report that an existing
rootless user namespace is required, use the same context through this fallback:

```sh
podman unshare hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
```

`--skopeo PATH` and `--podman PATH` select executables. Skopeo must support the
required shared-blob options, platform flags when selected, and digest
preservation for registry pull and every export.

## Import and export

Import copies a local image into the persistent store. A ref such as
`analysis/tool:release` is the name and tag used to select it. Export restores
the selected persistent image into local containers-storage.

```sh
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release
hpc-image-store --store /scratch/me/images import localhost/tool:latest analysis/tool:release --replace --notes 'updated build'
hpc-image-store --store /scratch/me/images --platform linux/arm64/v8 import localhost/tool:latest analysis/tool:arm64
hpc-image-store --store /scratch/me/images export analysis/tool:release localhost/restored:latest
hpc-image-store --store /scratch/me/images --json export analysis/tool:release
```

An omitted destination defaults to the source selector. Import and pull can
initialize a missing or empty store beneath its prepared parent. Export requires
an initialized store. An existing destination, including a damaged ref, requires
`--replace`; there is no automatic repair or replacement. Notes are optional
acquisition metadata.

Import may compress or convert an image, changing its manifest digest. The
stored root is its subsequent local digest identity. Every export requests
preservation of that stored manifest, even for imported images. Unsupported
capabilities or preservation failures are errors without fallback. Export
leaves persistent image data and metadata unchanged; partial destination data
is managed by containers-storage.

`--platform OS/ARCH[/VARIANT]` selects a platform without converting architecture.
Import/export use Skopeo defaults when it is omitted. A store path containing
`:` cannot be used for transfers because Skopeo's OCI transport cannot address it.

## Names and digests

Import, pull, export, and single-image check use `name[:tag][@digest]`. Supply a
repository name and at least a tag or digest. Registry ports work. Names retain
their spelling: `alpine` and `docker.io/library/alpine` are different repositories.
No `latest` tag or registry prefix is inferred. Bare digests, image IDs,
transport URLs, and embedded storage settings are rejected. Digests require
`sha256:` followed by 64 lowercase hexadecimal digits.

For a local lookup, a digest overrides any tag and searches committed refs in
the same literal repository. `analysis/tool:old@sha256:<R>` can select another
tag of `analysis/tool`, but never a different repository. Use the actual stored
root shown by inventory, not a config digest or original acquisition digest.
In the examples below, replace placeholders with complete digests.

```sh
hpc-image-store --store /scratch/me/images export 'analysis/tool@sha256:<stored-root>' localhost/restored:latest
hpc-image-store --store /scratch/me/images check 'analysis/tool@sha256:<stored-root>'
```

For import/pull naming, keep the destination tag if present. Otherwise derive a
tag from the first 12 hexadecimal characters of the explicit source digest.
A source tag resolved during pull cannot supply that generated tag. Explicit
acquisition destination digests are ignored and reported; they do not assert
the resulting root or supply a tag.

| Acquisition input | Committed name |
| --- | --- |
| `alpine@sha256:<D>`, destination omitted | `alpine:<first12(D)>` |
| Same source, destination `renamed@sha256:<E>` | `renamed:<first12(D)>`; `E` is ignored |
| Same source, destination `renamed:release@sha256:<E>` | `renamed:release`; `E` is ignored |
| `alpine:latest`, destination `renamed@sha256:<E>` | Invalid: the source supplies no digest for a tag |

Generated-tag collisions fail unless replacement is explicit. If import changes
source root `D` into stored root `R`, the tag still derives from `D` and later
digest lookup uses `R`. Export instead passes destination digests to Skopeo for
enforcement, and defaults to the exact supplied source selector.

## Pull from a registry

```sh
hpc-image-store --store /scratch/me/images pull docker.io/library/alpine:3.23
hpc-image-store --store /scratch/me/images --platform linux/arm64/v8 pull registry.example/tool:release analysis/tool:arm64 --notes 'job image'
hpc-image-store --store /scratch/me/images pull registry.example/tool:release --authfile /path/to/auth.json --cert-dir /path/to/certs --policy /path/to/policy.json
```

Pull stores one OCI manifest without populating containers-storage or invoking
Podman. Use export afterward for job-local use. `--authfile`, `--cert-dir`,
`--policy`, and `--tls-verify[=true|false]` apply to both source resolution and
copying. Omitted settings inherit Skopeo configuration and environment, including
`REGISTRY_AUTH_FILE`. Skopeo reads credentials; they are never saved in ref metadata.

A digest source must identify the image manifest itself. Index and manifest-list
digests are rejected even if they contain one image. A tag may resolve through
an OCI index or Docker manifest list to one manifest. Nested-index resolution
and Docker-specific stored image media types are unsupported. The selected
manifest is pinned by digest for copying, so later tag movement does not change
the acquisition. Its digest is preserved without conversion or weaker retries.

| Source | Platform behavior |
| --- | --- |
| Index tag without `--platform` | `linux/amd64` on an `x86_64` library target or `linux/arm64` on `aarch64`; other targets need an explicit platform |
| Index tag with `--platform` | Match OS and architecture, filtering variant only when supplied; require one distinct matching digest |
| Direct manifest without `--platform` | Accept its actual platform |
| Any source with `--platform` | Verify the completed image configuration matches the requested fields |

Multiple distinct matches or no match fail; descriptor order never breaks ties.
CPU variants are not detected. These defaults can differ from Skopeo defaults.
An index used for selection is neither stored nor retained as a lookup alias.
If a pull uses a different destination repository, subsequent local lookup uses
that destination's literal repository name.

## Inventory and checking

```sh
hpc-image-store --store /scratch/me/images list
hpc-image-store --store /scratch/me/images list --digests
hpc-image-store --store /scratch/me/images list --format full
hpc-image-store --store /scratch/me/images list --format json
hpc-image-store --store /scratch/me/images check analysis/tool:release
hpc-image-store --store /scratch/me/images --json check --all
```

The default inventory follows `podman images`: repository, tag, shortened OCI
config image ID, creation age, and stored content size. `--digests` adds the
full stored root. `--format full` exposes metadata, paths, status, and full
identities; JSON includes entries, diagnostics, and enumeration completeness.
Global `--json` selects JSON and conflicts with explicit non-JSON formatting.
`--digests` requires Podman formatting.

Rows are ordered newest first, with name/path tie-breaking. Sizes use decimal
units and three significant digits; IDs are shortened to 12 hexadecimal digits.

Creation time and size are cached display fields. Stored content size differs
from unpacked Podman size and cannot predict reclaimed space. Missing display
values appear as `<none>`. For an exported image already present under the same
config image ID, `podman images` may retain the existing display values; use
`podman image inspect` to inspect added names in `RepoTags`, `RepoDigests`, and
`NamesHistory`.

Inventory inspects small wrapper files without traversing image content.
Healthy entries are therefore `unverified`. Damaged entries remain visible,
and collected results are printed even when damage or incomplete enumeration
causes status 1. Non-UTF-8 paths have quoted byte-escaped representations.

Check accepts one selector or `--all` and traverses the supported local graph,
including encountered nested indexes and all their dependencies. It verifies
structure, supported types, file presence, and sizes. It does not hash blobs,
unpack layers, compare cached display fields against content, or certify execution.
Same-size corruption can remain undetected. Passing internal index checks does
not imply index acquisition or export support. Check never repairs or removes
content; it may create coordination bookkeeping. All-store inventory and checks
observe individual refs coherently without promising a store-wide snapshot.

## Remove refs and reclaim content

```sh
hpc-image-store --store /scratch/me/images remove analysis/tool:release
hpc-image-store --store /scratch/me/images prune --dry-run
hpc-image-store --store /scratch/me/images --json prune --dry-run --grace-period 7d
hpc-image-store --store /scratch/me/images prune
```

Removal accepts only an explicit `name:tag`, including for a damaged ref.
Digest-bearing selectors are rejected. It removes that logical name while
leaving its raw image and shared content for prune. It can initialize a missing
store beneath its prepared parent before reporting the ref missing.

Prune requires an initialized store in both modes. It performs a fresh complete
scan and preserves all content if unresolved committed-ref damage or an
operational inspection failure prevents safe deletion. Unknown contents are
preserved and reported as successful skips. Existing checks or availability
observations never authorize later pruning.

Content grace defaults to 24 hours of object age, not time since removal.
`--grace-period` accepts integer seconds with an optional `s`, `m`, `h`, or `d`
suffix, including `0`. Old content can become eligible immediately after its
last ref is removed. Future timestamps remain ineligible. Recognized unused ref
locks and empty ref/lock directories are eligible immediately regardless of
grace; committed locks and the permanent store lock remain.

Dry-run changes no persistent state. It does not attempt the namespace sync
barrier required before destructive pruning and cannot certify that a later
deletion will succeed. Real prune saves the scanned ref namespace before
reclaiming eligible content. It stops on the first removal or sync failure and
reports partial progress without rollback.

Reports show eligible candidates, visible removals, per-kind counts and logical
bytes, unknown-content skips, scan completeness, and diagnostics. Logical bytes
are not physical space savings. `durability_confirmed == false` means a removal
is visible but its parent sync failed. Review diagnostics as well as totals.

## Output and troubleshooting

Status 0 means success, 1 means an operation or collected check/report failed,
and 2 means invalid CLI usage. JSON results go to stdout, with diagnostics,
skips, and warnings on stderr. Tools' output is captured so it cannot corrupt
JSON. A store-preparation or selector-resolution failure may occur before any
JSON report exists. Successful acquisition can return cleanup warnings and
status 0 because the image was already durably published.

| Symptom | Action |
| --- | --- |
| Store cannot initialize | Check the prepared immediate parent and its open/sync permissions; missing ancestors are not created. |
| Destination already exists | Inspect it and choose an unused name or request explicit replacement. Availability is an observation, not a reservation. |
| Export cannot preserve content | Check Skopeo capability and representation support; no conversion fallback is available. |
| Digest cannot be resolved | Use a stored root under the same literal repository. Resolve damaged candidates before treating an incomplete search as absence. |
| Lock wait or timeout | Check active transfers, including surviving copy children. Do not delete lock files to break contention. |
| Prune is blocked | Inspect committed refs and diagnostics; explicitly remove or replace a damaged ref only when that is the intended recovery action. |
| Durability error after mutation | Inspect current state before retrying: publication or removal may already be visible. No rollback hides that uncertainty. |

Preserve unexpected contents when investigating errors. Replacing a ref cannot
generally repair damaged shared immutable objects. See the administration guide
for recovery limits and backup responsibilities.
