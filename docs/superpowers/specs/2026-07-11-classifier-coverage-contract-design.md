# Classifier Coverage Contract Design

**Date:** 2026-07-11

**Tracker:** `clarion-b5c50abb19`

**Decision:** The public-surface completeness gap spans Loomweave and
Plainweave. Loomweave will publish authoritative classifier support and
per-analysis completeness independently of observed tag instances. Plainweave
will use that producer metadata for denominator completeness and keep entity
tags as match evidence only.

## Problem

Loomweave currently derives tag availability from result cardinality. In
`tag_facet`, an empty result receives `signal.available=false`, while
`known_tags` is derived from the distinct tag values currently stored in
`entity_tags`. The contract therefore cannot distinguish a classifier that ran
successfully and matched zero entities from an unsupported or unavailable
classifier.

Plainweave 1.2.1 makes the corresponding consumer-side inference: it treats a
tag class as supported only when at least one entity carries that tag. On the
fresh Scrappack index this yields five real entry points and CLI commands, zero
HTTP routes, and zero explicit exports, but marks the HTTP-route and
exported-API denominator classes incomplete. The zero counts are legitimate;
the inability to prove why they are zero is the defect.

## Producer Contract

### Manifest declaration

Each language plugin declares the static tag classifiers it implements in its
`[ontology]` manifest table:

```toml
[ontology]
classifier_tags = [
    "cli-command",
    "entry-point",
    "exported-api",
    "http-route",
]
```

The field is optional with an empty default so existing third-party manifests
continue to parse. Values are normalized, sorted, deduplicated, and validated
as non-empty lowercase kebab-case tag names. A missing declaration means
"support unknown", never "supported".

Python declares its statically implemented classifiers: `cli-command`,
`data-model`, `entry-point`, `exported-api`, `framework-handler`, `http-route`,
`public-surface`, and `test`. Rust declares `allow-dead-code`, `cli-command`,
`entry-point`, `exported-api`, `framework-handler`, `http-route`, and `test`.

### Per-run coverage record

Every analysis run persists a versioned coverage object inside `runs.stats`:

```json
{
  "classifier_coverage": {
    "schema": "loomweave.classifier-coverage.v1",
    "source_walk_complete": true,
    "source_walk_skipped_entries": 0,
    "plugins": [
      {
        "plugin_id": "python",
        "plugin_version": "1.4.1",
        "ontology_version": "0.12.0",
        "matched_files": 32,
        "analyzed_files": 32,
        "retained_files": 0,
        "degraded_files": 0,
        "status": "complete",
        "classifier_tags": [
          "cli-command",
          "data-model",
          "entry-point",
          "exported-api",
          "framework-handler",
          "http-route",
          "public-surface",
          "test"
        ]
      }
    ]
  }
}
```

Plugin status is `complete`, `degraded`, `failed`, or `not-applicable`.
Incrementally retained files count as covered only when the stored plugin
version and ontology marker matches the live manifest; that is the existing
full-redispatch invariant. Syntax-degraded files count as degraded because the
Python extractor emits only a module anchor and cannot classify functions in
that file.

The latest run is authoritative. Missing coverage metadata, a non-completed
latest run, skipped source entries, or relevant degraded/failed plugins fails
closed.

### Catalog response

`entity_tag_list` and every tag-backed shortcut attach an always-present
classification block:

```json
{
  "classification": {
    "schema": "loomweave.classification.v1",
    "tag": "http-route",
    "state": "supported",
    "complete": true,
    "matches": 0,
    "supporting_plugins": ["python"],
    "unsupported_plugins": [],
    "run_id": "a7eb04c7-6bff-494f-8c35-b9403b7647d6",
    "run_status": "completed",
    "reasons": []
  }
}
```

`state` is one of:

- `supported`: every active source plugin declares and completed the classifier;
- `partial`: at least one active source plugin supports it and at least one does not;
- `unsupported`: active source plugins exist but none declares it;
- `unavailable`: no authoritative current-run coverage record exists.

`complete` is true only when state is `supported`, the latest analysis and all
relevant plugin coverage are complete, and `page.truncated`,
`scope_truncated`, and catalog `scan_truncated` are all false. Thus zero matches
can be supported and complete, while a nonzero result can remain incomplete.

The existing `entities`, `facet`, `known_tags`, `page`, `scan_truncated`,
`scope_truncated`, and `signal` fields remain. `signal.available` will track
classifier availability rather than result cardinality, and the versioned
`classification` block becomes the authoritative interpretation. Consumers
must not infer support from `known_tags`.

## Python Tag Semantics

- `exported-api` is explicit export evidence. `__all__` is authoritative;
  `__all__ = []` is supported-empty. Without `__all__`, public top-level
  definitions receive the lower-confidence `public-surface` tag instead.
- `http-route` recognizes supported decorator terminal names on every
  successfully parsed function. It is not conditional on importing FastAPI.
- `entry-point` covers module-level `main` and main-guard targets.
- `cli-command` covers qualifying main-guard, `sys.argv`/argparse, and
  command/group/callback decorator patterns.

These semantics remain unchanged. No sentinel entities or fabricated tags are
introduced.

## Consumer Migration

Plainweave's local SQLite adapter reads the latest `runs.stats` classifier
coverage record, joins it with observed `entity_tags`, and computes each
requested surface independently. Tags provide match rows and counts; the
coverage record provides support and analysis completeness.

Because Plainweave's current v1 intent-coverage schema uses exact-key contract
tests, per-class state should ship as a versioned v2 response or a negotiated
companion field. V1 may remain as a legacy projection, but its documentation
must stop defining tag-instance presence as classifier completeness.

## Verification

Regression coverage includes supported-zero, supported-nonzero, unsupported,
unavailable, degraded, mixed-plugin, pagination-truncated, scope-truncated, and
catalog-scan-truncated cases. The final gate runs Rust formatting, Clippy, the
workspace nextest suite, Python Ruff/format/Mypy/Pytest checks, and Wardline if
the implementation touches external-input parsing or validation.

The modified Loomweave build is then run against Scrappack. Expected source
facts remain five entry points, five CLI commands, zero HTTP routes, and zero
explicit exports; the two zero classes become provably supported-empty when
the Python analysis is complete.
