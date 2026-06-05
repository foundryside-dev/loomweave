# Descriptor-Backed Wardline Annotation Metadata

## Summary

Loomweave's Python plugin consumes Wardline's NG-25 trust-vocabulary descriptor
without importing Wardline. When the descriptor is available, the plugin records
source-observed Wardline decorator facts on Loomweave function/class entities as
metadata and tags. Wardline remains authoritative for vocabulary and policy
semantics; Loomweave stores only what it observes in source against that
descriptor.

## Design

The plugin resolves the descriptor once during `initialize`:

1. `<project_root>/.wardline/vocabulary.yaml`
2. installed Wardline distribution data file `wardline/core/vocabulary.yaml`
3. absent/degraded state

Descriptor resolution uses package metadata and file reads only. The plugin
must not import `wardline`, `wardline.core`, or `wardline.core.registry` on the
startup path.

`capabilities.wardline` reports `enabled`, `version_skew`, or `absent`. Missing,
unreadable, malformed, or duplicate-entry descriptors do not abort analysis.
Normal structural extraction continues and no Wardline entity metadata is
emitted.

For matched decorators, the plugin attaches a `wardline` object to the emitted
entity:

- `descriptor_version`
- `confidence_basis` (`descriptor` or `descriptor_version_skew`)
- `decorators[]` with canonical name, qualified source name, group, attrs, and
  line

The same entity gets denormalized tags: `wardline` and
`wardline:<canonical_name>`.

Decorator matching uses only the final qualified-name segment. This supports
bare, imported, and qualified forms without interpreting decorator arguments.

## Non-Goals

- No Rust storage schema change.
- No first-class decorator entities or decorator edges.
- No taint-level interpretation of decorator arguments.
- No Filigree finding emission for ordinary decorator observations.

## Verification

- Unit tests cover descriptor resolution, invalid descriptors, duplicates,
  version skew, and no-import regression behavior.
- Extractor tests cover direct, qualified, stacked, absent, and version-skew
  decorator metadata.
- Server tests cover initialize capabilities and vocabulary threading into
  `analyze_file`.
- Manifest/version guard tests cover `wardline_aware=true`, the descriptor pin,
  and ontology/version lockstep.
