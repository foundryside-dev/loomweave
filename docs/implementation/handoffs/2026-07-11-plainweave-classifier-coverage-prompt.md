# Ready-to-Paste Plainweave Task: Supported-Zero Intent Coverage

Use the following prompt in `/home/john/plainweave`:

---

## Objective

Fix Plainweave intent coverage so denominator completeness comes from
Loomweave's authoritative per-classifier support and analysis-completeness
metadata, not from whether at least one entity currently carries each tag.

The key rule is: **zero matches is complete when a supported classifier ran
successfully and enumeration was complete.** Do not fabricate entities or tags.

## Producer contract

Loomweave is adding a versioned object to the latest `runs.stats` row:

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

Loomweave's tag-backed MCP responses will also expose:

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
    "run_id": "...",
    "run_status": "completed",
    "reasons": []
  }
}
```

Plainweave's current boundary is local-only SQLite, so consume the persisted
`runs.stats.classifier_coverage` record rather than introducing an implicit
live MCP call. Treat missing or malformed metadata as unavailable and fail
closed.

## Current defect

Plainweave 1.2.1 currently:

- opens the local Loomweave database in
  `src/plainweave/loomweave_adapter.py:118-125`;
- derives `present_tags` from `SELECT DISTINCT tag FROM entity_tags` at
  `loomweave_adapter.py:133-142`;
- derives only row-producing plugin IDs at `:143-148`;
- computes absent classes as `PUBLIC_SURFACE_TAGS - present_tags` and marks
  coverage complete only when that set is empty at `:196-204`;
- propagates this boolean to `denominator_complete` in
  `src/plainweave/service.py:1612-1663`.

That conflates classifier capability with observed cardinality. A supported
HTTP-route classifier returning zero routes looks identical to an unsupported
classifier.

## Required behavior

For each requested surface class (`cli-command`, `entry-point`,
`exported-api`, `http-route`), calculate:

- `supported-complete`, including zero matches;
- `partial` when only some active source plugins support it;
- `unsupported` when active source plugins exist but none declares it;
- `unavailable` when current authoritative producer metadata is absent;
- `incomplete` when the source walk or any relevant plugin is degraded/failed.

Observed `entity_tags` remain the match/count evidence. They never establish
support by themselves. `present_plugins` also does not establish classifier
support.

Denominator completeness is true only if every requested class is
supported-complete. Missing capability metadata, malformed JSON, a non-completed
latest run, `source_walk_complete=false`, degraded/failed relevant plugin
coverage, or any producer enumeration truncation keeps it false.

Keep Plainweave's `surfaces_truncated` meaning separate: it caps returned
justified/unjustified evidence rows and does not change the already-computed
denominator counts.

## Compatibility

The current `weft.plainweave.intent_coverage.v1` and catalog contracts have
exact-key validators. Do not silently add fields to v1. Prefer either:

1. a negotiated/versioned `weft.plainweave.intent_coverage.v2` carrying
   per-class state; or
2. an additive companion read while preserving v1 as a documented legacy
   projection.

Update PDR-009 and product metrics wording: completeness means classifier
support plus complete enumeration, not the presence of one instance from every
tag class.

## Acceptance tests

Add focused tests for all of these cases:

1. Python coverage declares `http-route`, zero route tags: supported-complete,
   count zero, denominator not degraded for that class.
2. Supported HTTP classifier plus a real FastAPI route: route locator, SEI, and
   source anchor are preserved.
3. Python coverage declares `exported-api`, zero export tags: supported-complete.
4. Explicit export evidence: exported entity is returned.
5. Active unsupported plugin: incomplete/unsupported, never complete-zero.
6. Relevant plugin absent or coverage record absent: unavailable and fail closed.
7. Syntax-degraded relevant file: supported but incomplete.
8. Source, catalog, or scope truncation: completeness false regardless counts.
9. Observed tag rows without capability metadata do not establish support.
10. Scrappack regression: five entry points and CLI commands, zero HTTP routes
    and explicit exports, with the two zero classes still knowably supported
    after a complete Python scan.

Run the narrow adapter and intent-coverage tests, the contract/schema tests,
and Plainweave's full canonical verification suite. Do not modify Loomweave or
Scrappack during this task.

## Final report

Report the old inference, the new authoritative fields, schema/version choice,
tests added, commands run, and the before/after Scrappack coverage response.

---
