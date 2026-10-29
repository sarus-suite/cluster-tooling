# Deferred validation and qualification

The first PoC closes with existing crate checks, one local lifecycle scenario,
and documentation consolidation. This is a first pass, not exhaustive validation
or production acceptance. The topics below retain the intent of broader
qualification without providing external acceptance procedures or prescribing
the next implementation phase. Current evidence belongs in the
[development guide](development.md); required behavior belongs in the
[architecture](architecture.md).

| Follow-up topic | Evidence it would add |
| --- | --- |
| Disposable real registry | Direct manifests, index-based tag selection, index-digest rejection, tag movement, and authentication against an actual registry, beyond controlled tool fixtures and the historical opt-in Docker Hub smoke. |
| Registry configuration and representations | Broader authentication, TLS, trust policy, tool-version, media-type, compression, and platform combinations; clear preservation failures without conversion or fallback. |
| Integrated registry lifecycle | Availability before/after pull, export, removal, checking, and pruning together, including unusable content and operational failures. |
| Two-node Lustre/VAST coordination | Concurrent initialization beneath a prepared parent, lock exclusion, shared content insertion, publication visibility, and coherent observations across replacement/removal. |
| Interrupted initialization and transfers at deployment sites | Retry behavior, stable lock identities, surviving tool children, abandoned staging, and prune exclusion on the actual filesystems. |
| Crash persistence on deployed storage | Ref/metadata publication, failed-sync outcomes, namespace persistence before reclamation, and survival of acknowledged updates across actual crash/recovery. Local ordering and fault injection do not establish these guarantees. |
| Availability and pruning across nodes | Observation races, active-reader exclusion, and fresh authoritative reclamation decisions independent of earlier availability results. |

These topics require deployment-specific environments and separately agreed
scope. Lustre and VAST support remains unqualified until suitable site evidence
exists. No change should silently substitute weaker primitives to make a
qualification scenario pass.

Complete-index acquisition/storage, multi-platform stored images, nested-index
source resolution, index-parent inventory, and platform-selecting index export
are unsupported capabilities, not unfinished PoC acceptance checks. They require
future implementation decisions before their own qualification. Existing internal
index graph tests protect checking and pruning safety without claiming those
public workflows.
