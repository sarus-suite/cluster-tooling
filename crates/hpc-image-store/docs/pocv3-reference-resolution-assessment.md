# Deferred PoC v3 assessment: reference resolution and Sarus integration

## Status and motivation

This assessment was recorded on 29 October 2026 for future PoC v3 planning.
The work is deferred and outside [PoC v2](pocv2-implementation-plan.md). It is
neither an approved implementation plan nor a claim that the integration is
already supported. The [architecture](architecture.md) remains authoritative
for current store behavior.

The intended future direction is for hpc-image-store to become the only image
store visible through `sarusctl` and the source of truth for a Sarus Suite
installation. Podman and parallax stores would become caches populated from
that authority. Replacing the current Podman pull workflow would require
familiar reference resolution as well as changes to cache population and
lookup. Short-name expansion alone does not complete that integration.

The recommendation is to finish PoC v2's coordination, complete mutations,
recovery, and existence work before introducing these naming and integration
contracts. PoC v2's selector semantics remain unchanged by this assessment.

## Current implementation and inherited Skopeo behavior

The store's `ImageSelector` requires a tag or digest, preserves spelling, and
does not supply an implicit `latest`, registry, or namespace. Local lookup
uses literal repository names. `Store::pull` selects its local destination
before remote inspection, then passes `docker://...` references to Skopeo for
inspection and digest-pinned copying. See [selectors](../src/identity.rs),
[pull](../src/pull.rs), and [destination naming](../src/transfer.rs).

Today, [sarusctl's pull workflow](../../sarusctl/src/lib.rs) delegates acquisition
to Podman, checks Podman storage, and then migrates the image into parallax.
The run workflow also consults parallax to decide whether acquisition is needed.
Both decisions would need to change when the persistent store becomes the
authority.

Skopeo uses containers/image, but different parts of that library implement
different aspects of registry configuration. Reading `registries.conf` does
not mean that a transport invokes every resolution feature in that file.

| Feature | Behavior of the Skopeo path used by the store |
| --- | --- |
| Docker-style normalization | `docker://alpine:3.23` selects `docker.io/library/alpine:3.23`. Names without a tag or digest can gain `latest` at the transport level, although the store parser rejects them first. |
| Registry remapping and mirrors | Matching registry settings can redirect access to another location or try mirror endpoints. The requested logical repository remains distinct from the physical download endpoint. |
| Podman short-name aliases and search registries | Direct `docker://` parsing does not invoke the separate alias/search resolver, prompt for a registry, or record a remembered short-name selection. |

These conclusions follow from Skopeo's
[transport parsing](https://github.com/containers/skopeo/blob/main/cmd/skopeo/utils.go),
the Docker transport's
[reference parsing](https://github.com/containers/image/blob/main/docker/docker_transport.go),
[Docker normalization](https://github.com/containers/image/blob/main/docker/reference/normalize.go),
and [source endpoint selection](https://github.com/containers/image/blob/main/docker/docker_image_src.go).
Podman's configurable selection is implemented by the separate
[short-name resolver](https://github.com/containers/image/blob/main/pkg/shortnames/shortnames.go).

For example, setting `unqualified-search-registries = ["quay.io"]` does not
make `skopeo inspect docker://alpine:3.23` select `quay.io/alpine:3.23`.
A mirror configured for `docker.io/library`, however, can serve that logical
Docker Hub reference. This is an explanatory example, not a recorded live
registry test.

Therefore, do not plan a second mirror implementation in Rust. Mirror and
remapping behavior is already inherited from Skopeo; deployment qualification
and integration tests are separate work. Registry policies can make endpoints
eligible only for tags or only for digests. Consequently, the store's initial
tag inspection and later digest-pinned copy can use different endpoints while
retaining the selected content digest. See the
[registry configuration reference](https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md).

This assessment is based on repository and upstream source/documentation
inspection, not an installed-version compatibility or deployment test. The
linked upstream `main` branches can change; recheck them against the Skopeo and
containers/image versions selected for the future milestone.

## Complexity and milestone recommendation

Podman resolution includes configured aliases, ordered search registries,
resolution modes, terminal-dependent prompting, and remembered choices. Missing
tags default to `latest`; Docker Hub supplies `library/` only when no namespace
is present. See [Podman pull](https://docs.podman.io/en/latest/markdown/podman-pull.1.html)
and the registry configuration reference above. An arbitrary default namespace
for another registry would be a separate Sarus policy, not a universal Podman
normalization rule.

| Possible scope | Complexity assessment | Planning implication |
| --- | --- | --- |
| One explicit default registry, Docker Hub namespace normalization, and implicit `latest` | Low to moderate | A bounded feature, but it still changes local identity and lookup expectations. It remains deferred here. |
| Configured aliases and multiple search registries with deterministic noninteractive behavior | Moderate to high | Prefer a separate milestone with explicit ambiguity and failure rules. |
| Podman-compatible configuration precedence, prompting, remembered aliases, and local resolution | High | Treat as a PoC v3 workstream rather than a small extension to pull. |

These are qualitative scope estimates, not delivery-time commitments. The
central cost is consistent identity across actions and caches, rather than
constructing a longer string. The upstream resolver also provides different
entry points for acquisition and local lookup. Reproducing pull resolution
does not automatically define how exists, export, check, remove, and run should
resolve the same input.

## Candidate work outline for PoC v3

The following sequence is a starting point for planning, subject to design
decisions and a separately authorized implementation milestone.

1. **Choose the compatibility and configuration policy.** Decide whether to
   support deterministic Sarus defaults, a documented Podman subset, or full
   Podman resolution. Specify installation versus user configuration, aliases,
   precedence, ambiguity, noninteractive jobs, and whether prompting and learned
   aliases are supported. Define when registry search may continue after an
   error so that authentication or policy failures do not silently select a
   different image.
2. **Separate input resolution from stored identity.** Introduce a shared
   resolution capability used by the CLI and library callers, rather than
   implementing it only in argument handling. Distinguish requested spelling,
   qualified logical repository, selected manifest digest, and explicit local
   destination. Evaluate reuse of the upstream Go resolver versus a documented
   Rust subset or helper; the existing Skopeo transport path is not itself an
   interface to Podman's alias/search resolver.
3. **Specify consistent local lookup.** Define how short and qualified inputs
   behave for exists, export, check, remove, and run, including ambiguous stored
   matches, digest scoping, locally imported images, and configuration changes.
   Local existence and removal must not require remote probes or pulling data.
   Determine compatibility for existing literal short refs before changing
   their meaning or introducing aliases.
4. **Integrate acquisition with stable resolution.** Choose the qualified
   repository once, resolve and pin the manifest, and retain both through
   inspection and copying. Preserve explicit local destination names. Reuse
   Skopeo for registry policy, authentication, mirrors, and remapping. Decide
   which resolved identities belong in reports and provenance and whether
   metadata or format changes are needed.
5. **Make sarusctl use the persistent authority.** Route acquisition, inventory,
   existence, and removal through hpc-image-store. Populate Podman/parallax
   caches from the selected stored content; do not use their contents to choose
   the meaning of a name or prove persistent availability. Specify cache
   identity, refresh, removal, and failure behavior independently from the
   authoritative ref lifecycle.
6. **Qualify the supported behavior and document differences.** Add the test
   cases below and publish the supported compatibility subset and configuration
   rules. Keep any unsupported Podman behavior explicit.

A recommended direction for discussion is to store fully qualified logical
names by default for remote acquisitions, retain original input for diagnostics
and provenance, and keep explicitly requested local destination names distinct.
Physical mirror addresses should not become the user-visible image identity.
This is a recommendation, not a decided PoC v3 storage contract.

Full replacement of `podman pull` also requires separate decisions about
repeated-pull/update policy and supported media types, platforms, and transports.
The current store requires explicit replacement and supports a narrower set of
acquisition workflows than Podman. Do not imply that implementing expansion
alone delivers complete pull compatibility or authorize these broader changes
without planning them.

## Candidate validation and open decisions

Use synthetic or controlled fixtures for name resolution and a disposable
registry setup for endpoint behavior. Candidate cases include:

- Explicit registries, hosts with ports, `localhost`, short names with and
  without namespaces, tagless inputs, and digest-qualified selectors.
- Docker Hub's conditional `library/` insertion, alias precedence, multiple
  registry ambiguity, and supported terminal/nonterminal behavior.
- Matching identities across pull, exists, export, check, remove, and run;
  explicit destination names and configuration changes after acquisition.
- Stable repository and digest selection despite tag movement, mirror
  fallback, remapping, and tag-only versus digest-only mirror eligibility.
- Offline local lookups and cache eviction/repopulation without losing the
  authoritative image or allowing a stale cache to select different content.
- Permission, authentication, policy, and registry failures that preserve
  useful errors and do not unexpectedly change the selected source.

Before turning this outline into an implementation plan, resolve the supported
Podman subset, configuration ownership, local naming and compatibility rules,
resolver reuse strategy, repeated-pull policy, and cache identity contract.
No new PoC v2 stage, source change, or validation requirement is introduced by
recording this assessment.
