# ADR-018: Identity Reconciliation — Clarion Translates; Wardline Owns Its Qualnames

**Status**: Accepted (Revision 3, 2026-06-05 — direct REGISTRY import asterisk retired; Python plugin reads Wardline's NG-25 descriptor without importing Wardline.)
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: three independent identity schemes exist across Clarion, Wardline, and Wardline's exception register; one-way translation is the federation-compatible answer

## Summary

Clarion maintains the translation layer between its own `EntityId` scheme (ADR-003) and Wardline's qualnames / exception-register locations. Wardline is authoritative for its own qualnames and does **not** adopt Clarion's ID format. The Python plugin imports `wardline.core.registry.REGISTRY` directly at startup with a `REGISTRY_VERSION` pin; skew is handled with three graduated responses (exact / additive / major-bump) rather than hard failure. The HTTP read API exposes an `entities/resolve?scheme=…&value=…` oracle so siblings ask in their own scheme instead of embedding Clarion's ID format. The direct-import pattern is an **initialization coupling** (`loom.md` §5 asterisk 2) scoped to the Wardline-aware plugin specifically; the Clarion core and any non-Wardline-aware plugin do not depend on Wardline being importable.

## Context

Three identity schemes coexist and none are byte-equal for the same underlying symbol (`detailed-design.md:557-561`):

| Scheme | Example | Owner | Format |
|---|---|---|---|
| Clarion `EntityId` | `python:class:auth.tokens::TokenManager` | Clarion | `{plugin_id}:{kind}:{canonical_qualified_name}` |
| Wardline `module` + `qualified_name` | `src/auth/tokens.py` + `TokenManager.verify` | Wardline `FingerprintEntry` | Source file path plus Python's bare `__qualname__`; not byte-equal to Clarion's dotted-module-prefixed `qualified_name` |
| Wardline exception-register `location` | `src/wardline/scanner/engine.py::ScanEngine._scan_file` | `wardline.exceptions.json` | `{file_path}::{qualname}` |

Any cross-tool query — "which Filigree findings attach to this Clarion entity?", "which Wardline exception covers this qualname?" — has to bridge at least two of these. The ADR-003 decision to use symbolic canonical names produces Clarion's scheme but leaves the translation question open.

The Wardline integration adds a second dimension: Clarion's Python plugin depends on Wardline's decorator vocabulary (`REGISTRY`) to detect annotations correctly. Two approaches existed before this ADR — heuristic name-matching against a hardcoded list, or a direct Python import of the vocabulary. The design picked direct import (`system-design.md:949`) with `REGISTRY_VERSION` pinning, but the *why* (and the retirement conditions) weren't formalised.

The Loom doctrine is deliberate about identity (`loom.md` §6): Loom is **not** an identity-reconciliation service. "When cross-scheme translation is needed — e.g. Wardline qualname → Clarion entity ID — the product that *cares* does the translation, because that product is the one whose authority needs it." Clarion cares because Clarion owns the catalog that makes Wardline's qualnames meaningful to anything other than Wardline itself.

The 2026-04-17 panel's doctrine synthesis (`11-doctrine-panel-synthesis.md`) flagged the direct-import pattern as an explicit federation asterisk needing a retirement condition, not a quiet dependency. `loom.md` §5 now carries that asterisk: initialization coupling scoped to the Wardline-aware plugin specifically; non-Wardline-aware plugins and the Clarion core remain Wardline-independent.

## Decision

### Translation direction and authority

- **Inbound translation only.** Clarion translates Wardline qualnames, exception-register locations, and SARIF logical locations into `EntityId`s. Clarion does not push its ID scheme outbound — Wardline's emissions remain in Wardline's format, and Clarion's translation layer maps them at ingest.
- **Wardline is authoritative for its own qualnames.** `FingerprintEntry.qualified_name` and its paired `module` path are Wardline's contract; Clarion reads them and maps them. A future Wardline refactor that changes either field's format is a Wardline-side decision Clarion adapts to; the inverse is not true.
- **Reverse mapping is recorded, not pushed.** `WardlineMeta.wardline_qualname` on each Clarion entity property is the reverse lookup cache. It lives on Clarion's entity, not on Wardline's side, and is re-computed on every `clarion analyze`.

### 2026-05-18 amendment: asymmetric qualname storage is canonical

Sprint 1 verification proved that the two products encode the same Python symbol
with different field boundaries:

- Clarion's Python plugin emits `qualified_name =
  "{dotted_module}.{python.__qualname__}"` on each entity, e.g.
  `auth.tokens.TokenManager.verify`.
- Wardline's `FingerprintEntry` stores `module` as the source file path and
  `qualified_name` as Python's bare `__qualname__`, e.g.
  `src/auth/tokens.py` plus `TokenManager.verify`.

Direct string equality between Clarion's `qualified_name` and Wardline's
`qualified_name` is therefore forbidden for joins. Wardline-to-Clarion
translation composes Clarion's dotted module name from Wardline's `module`
using the same rules as the Python plugin's `module_dotted_name()` (`src/`
prefix stripped, `.py` removed, and `__init__.py` collapsed), then appends
Wardline's bare `qualified_name`. Clarion-to-Wardline translation must prefer
the recorded `WardlineMeta` / translator output; naively splitting on dots is
unsafe because dotted class chains and `<locals>` markers are part of Python's
`__qualname__`, not the module path.

### 2026-05-29 amendment: Wardline emits the dotted qualname pre-composed

The 2026-05-29 Wardline ↔ Loom integration brief (§4.A) resolves the asymmetric-storage clash **in Clarion's favor at the emission boundary**. Under the generic Wardline rebuild's native Filigree emitter ([ADR-015](./ADR-015-wardline-filigree-emission.md) Revision 2), each emitted finding carries `metadata.wardline.qualname` as the **combined dotted `module.qualified_name`** (e.g. `auth.tokens.TokenManager.issue`) — Clarion's L7 form, not Wardline's `(file-path module, bare qualname)` storage pair.

Two consequences:

1. **A clarified entry point.** This is a fifth reconciliation surface alongside the four below: Clarion reads `metadata.wardline.qualname` off a **Filigree finding** (via the federation read path / `issues_for` enrichment), not off a Wardline state file. The composition rule reverses — the 2026-05-18 amendment's "Clarion composes the dotted module name from Wardline's `module`" becomes "**Wardline emits it pre-composed; Clarion matches by direct qualname equality**" (`find_entity` by qualname). The state-file entry points (1–3) and their composition rule remain unchanged for any path that still reads Wardline's on-disk artifacts (e.g. historical SARIF baselines).

2. **The normalization contract is now Wardline's, at the emission boundary.** Because Wardline now performs the dotting Clarion used to do, byte-equality depends on Wardline replicating `module_dotted_name()` **exactly**: `src/` prefix stripped, `.py` removed, `__init__.py` collapsed (and the analogous handling for namespace-package / non-`src` layouts), while preserving `<locals>` markers and dotted class chains in `__qualname__` verbatim — those are part of the qualname, not the module path, and must not be re-dotted or stripped. If Wardline's composition diverges, reconciliation degrades silently to `resolution_confidence: heuristic | none` on exactly the nested-class and closure entities where it is least recoverable. Clarion exposes the canonical rules via `module_dotted_name()` and the `GET /api/v1/entities/resolve?scheme=wardline_qualname` oracle; a divergence shows up there as a non-`exact` resolution. This contract is the open ask-back to Wardline recorded in ADR-015 Revision 2; it is an enrichment-quality concern and does not gate the (Wardline, Filigree) transport path.

### Translation entry points (v0.1)

1. **`wardline.fingerprint.json`**: for each `FingerprintEntry`, compute `(module, qualified_name) → EntityId` using Wardline's `module_file_map` (from `ScanContext`). Write `WardlineMeta.wardline_qualname = qualified_name` on the resolved Clarion entity.
2. **`wardline.exceptions.json`**: parse `location` as `file_path::qualname` (split on the first `::`); same mapping rule. Unresolvable entries emit `CLA-INFRA-WARDLINE-EXCEPTION-UNRESOLVED` and persist as dangling records with `entity_id: null`.
3. **`wardline.sarif.baseline.json`**: use `location.logicalLocations[].fullyQualifiedName` when present, or `partialFingerprints` as a fallback. Unresolvable SARIF results carry `metadata.clarion.unresolved: true` through translation.
4. **`GET /api/v1/entities/resolve?scheme=<scheme>&value=<value>`**: HTTP read-API oracle. Schemes accepted: `wardline_qualname`, `wardline_exception_location`, `file_path`, `sarif_logical_location`. Response carries `resolution_confidence` (`exact | heuristic | none`) plus candidates for non-exact matches. 404-like misses return 200 with `resolution_confidence: "none"` to distinguish "Clarion doesn't know" from "Clarion is down."

### Direct REGISTRY import with REGISTRY_VERSION pin

The Python plugin imports `wardline.core.registry.REGISTRY` at startup. The `wardline` package is a dependency declared in the plugin's pipx venv. Skew behaviour (REQ-INTEG-WARDLINE-01, NFR-COMPAT-02):

| Installed `REGISTRY_VERSION` vs pinned | Behaviour |
|---|---|
| Exact match | Normal operation |
| Additive-newer (same major, same or higher minor) | Proceed with warning; decorators in the installed REGISTRY beyond the pin detected with `confidence_basis: clarion_augmentation`. Emit `CLA-INFRA-WARDLINE-REGISTRY-ADDITIVE-SKEW` |
| Major-bump or older | Fall back to hardcoded registry mirror (`wardline_registry_v<pin>.py`). Findings carry `confidence_basis: mirror_only`. Emit `CLA-INFRA-WARDLINE-REGISTRY-MIRRORED` |
| Wardline package not installable in plugin venv | Mirror mode from startup (same `MIRRORED` emission); `--no-wardline` declares the intent explicitly |

Pin policy: `REGISTRY_VERSION` is updated at Clarion release time alongside the hardcoded mirror. A Wardline minor bump between Clarion releases degrades to additive-skew (safe); a major bump degrades to mirror mode (safe but lossy).

### Why plugin-level, not core-level

The REGISTRY import is a property of the Wardline-aware plugin specifically, not of the Clarion core (`loom.md` §5 asterisk 2). The Rust core has no import path to Wardline; it's the Python plugin's startup that walks `sys.path` to load `wardline.core.registry`. The asterisk is named in `loom.md` §5 with an explicit retirement condition: when Wardline publishes a YAML/JSON descriptor export of its REGISTRY (NG-25, v0.2), non-Python plugins can consume it without a Python import, and the initialization coupling retires to a plain file-descriptor read.

This preserves the federation test: removing Wardline breaks Wardline-derived annotation detection but does not prevent the Clarion core from starting, does not prevent non-Wardline-aware plugins from running, and does not alter the meaning of Clarion's own catalog entries.

### Revision 3 (2026-06-05): direct-import asterisk retired

Wardline now publishes the NG-25 trust-vocabulary descriptor as `vocabulary.yaml`
and through `wardline vocab`. Clarion's Python plugin consumes that descriptor
instead of importing `wardline.core.registry.REGISTRY`. Resolution is
project-local `.wardline/vocabulary.yaml` first, then the installed Wardline
distribution data file `wardline/core/vocabulary.yaml`; both paths are plain
file reads and neither imports `wardline`, `wardline.core`, or
`wardline.core.registry`.

The plugin records source-observed decorator facts on Clarion entities as
Wardline metadata and `wardline:*` tags. Wardline remains authoritative for the
vocabulary and policy semantics; Clarion stores only what it parsed from source
against the descriptor. Missing or invalid descriptors continue to degrade
honestly: normal structural extraction proceeds, `capabilities.wardline` reports
the degraded state, and no Wardline entity metadata is emitted.

This closes the `loom.md` §5 initialization-coupling asterisk on the Clarion
side. The identity translation rules and `REGISTRY_VERSION`/descriptor-version
skew posture remain bilateral compatibility concerns; the load-bearing change is
that plugin startup no longer requires Wardline to be importable.

## Alternatives Considered

### Alternative 1: Wardline adopts Clarion's entity-ID scheme

Wardline changes its `FingerprintEntry.qualified_name` to carry Clarion's `{plugin_id}:{kind}:{canonical_qualified_name}` format directly.

**Pros**: zero translation layer; one identity scheme across the suite; cross-tool queries are string-equal comparisons.

**Cons**: Wardline can no longer stand alone — its internal data carries a format whose meaning depends on Clarion's ID-generation conventions. `loom.md` §6 prohibition "no identity reconciliation service" is violated: the scheme *is* a reconciliation service, embedded in one product's data. If Clarion changes its kind vocabulary or qualname normalisation, Wardline's historical data silently reinterprets. Wardline must re-analyze every commit Clarion has ever indexed to keep its IDs coherent.

**Why rejected**: it imports Clarion's authority into Wardline. The solo-use failure mode ("Filigree + Wardline without Clarion") breaks — Wardline cannot emit stable IDs without knowing Clarion's ID conventions.

### Alternative 2: Wardline qualnames become the suite-wide canonical identity

Clarion adopts Wardline's qualname format as its entity ID.

**Pros**: Wardline already computes them deterministically; the existing format is proven in production.

**Cons**: Python-specific (`TokenManager.verify` has no Java or Rust analogue that Wardline could produce); entity kinds Wardline doesn't scan — `file`, `subsystem`, `guidance` — have no qualname at all. Clarion's multi-language roadmap breaks the moment a Java plugin emits an entity with no qualname-shaped identity.

**Why rejected**: specialisation to one language + missing coverage for core-owned kinds. Same `loom.md` §6 violation inverted — now Wardline's authority is imported into Clarion.

### Alternative 3: Loom identity reconciliation service

A neutral Loom-level service maintains a translation table that every product queries.

**Pros**: symmetric; no product "owns" identity.

**Cons**: violates `loom.md` §6 ("Loom is not an identity reconciliation service") categorically, and §5 pipeline-coupling (the (Wardline, Filigree) pair composes only through a Loom mediator). Introduces the stealth-monolith pattern Loom exists to prevent.

**Why rejected**: categorical doctrine violation. The test for adding Loom-level services ("if the proposal introduces something that would need to be running or present for the suite to work, it violates federation") fails immediately.

### Alternative 4: Heuristic-only reconciliation (no REGISTRY import; string matching)

Clarion's plugin does not import Wardline's REGISTRY. Decorator detection uses a hardcoded list of known Wardline decorator names, updated in Clarion releases.

**Pros**: no initialization coupling. Clarion's plugin starts without Wardline present.

**Cons**: every Wardline decorator addition creates a drift window — the decorator lands in Wardline, but Clarion's plugin doesn't learn about it until the next Clarion release. For a v0.1 suite where Clarion and Wardline ship on independent cadences (and are the same author), the drift window is real and silent. The whole point of the REGISTRY is to be the shared vocabulary; refusing to import it re-creates the drift the REGISTRY was supposed to eliminate.

**Why rejected**: trades a named, retirement-conditioned initialization coupling (this ADR) for silent vocabulary drift. The named coupling is strictly preferable.

### Alternative 5: File-descriptor read of a YAML/JSON REGISTRY descriptor

Wardline exports its REGISTRY as a declarative descriptor file (`wardline_registry.yaml` or similar). Clarion's plugin reads the descriptor at startup instead of importing Python.

**Pros**: language-neutral — works for non-Python plugins. No Python import dependency. Cleaner federation shape (plain file-descriptor consumption).

**Cons**: Wardline has no such descriptor export today. Adding it is within-scope but is Wardline-side work the v0.1 Clarion plan does not commit to. NG-25 already names it as a v0.2 Wardline prerequisite ("YAML/JSON descriptor of REGISTRY enables non-Python plugins"), and the asterisk's retirement condition in `loom.md` §5 is exactly this.

**Why rejected for v0.1**: premature. v0.1 ships Python-only, so the Python-import path is sufficient and cheaper. The descriptor is the documented v0.2 path; the asterisk retires when it lands.

## Consequences

### Positive

- Translation is load-bearing for exactly one product (Clarion) in exactly one direction (inbound). Wardline and Filigree don't know translation happens.
- `REGISTRY_VERSION` pin with graduated skew response (exact / additive / mirror) means install skew degrades gracefully rather than hard-failing. An operator with a half-updated dev environment still gets useful output.
- The `entities/resolve` HTTP oracle exposes translation to siblings without requiring them to embed Clarion's ID format. Wardline's v0.2 HTTP client uses it; Filigree MCP calls already use its spiritual equivalent.
- Reverse mapping (`WardlineMeta.wardline_qualname`) enables "what Wardline qualname corresponds to this entity?" without re-running the reconciliation, and it lives on Clarion's data, not Wardline's.

### Negative

- Initialization coupling is named but not eliminated. Clarion's Python plugin cannot start without `wardline` installed in its venv; the coupling retires when NG-25 (YAML/JSON REGISTRY descriptor) lands in v0.2. The asterisk lives in `loom.md` §5 with that condition. An operator who installs Clarion without Wardline gets a plugin that runs in mirror mode from startup, which is noisy but functional; a misinstalled venv (wrong Python, missing dep) produces a clear startup failure with `CLA-INFRA-WARDLINE-REGISTRY-MIRRORED` as the first signal.
- Heuristic reconciliation (file moves, symbol renames not tracked by EntityAlias — ADR-003 names the v0.1 limitation) produces `resolution_confidence: heuristic` or `none` results. Operators see `CLA-INFRA-WARDLINE-EXCEPTION-UNRESOLVED` and run `clarion analyze --repair-aliases` manually.
- Three-scheme translation is quadratic in failure modes — the `resolve` oracle has to know every scheme, and every scheme has its own unresolvable case. Tractable at v0.1 scale (four schemes) but bears watching as siblings grow.

### Neutral

- The translation layer lives entirely in Clarion — plugin-side for REGISTRY import and qualname mapping, core-side for the `entities/resolve` oracle. No Wardline or Filigree changes are required by this ADR.
- The v0.2 YAML/JSON REGISTRY descriptor simplifies this ADR rather than replacing it: the import path becomes a file read, but the translation layer and the `REGISTRY_VERSION` pin semantics are unchanged.

## Related Decisions

- [ADR-002](./ADR-002-plugin-transport-json-rpc.md) — the subprocess transport is where the plugin's startup REGISTRY import happens. ADR-021's path jail does not apply (the import walks `sys.path`, not plugin-emitted paths).
- [ADR-003](./ADR-003-entity-id-scheme.md) — Clarion's `EntityId` format is the target of every translation entry point here. The v0.1 limitation ADR-003 names (symbol renames without file move) is the specific case this ADR's heuristic fallback covers.
- [ADR-015](./ADR-015-wardline-filigree-emission.md) — the SARIF translator is translation entry point 3. ADR-015 decides the translator's Wardline role (v0.1 bridge, retiring in v0.2); this ADR's translation rules apply for as long as the translator exists for any SARIF source.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — the plugin's REGISTRY import is an import, not a file read, so ADR-021's path jail does not apply. The `RLIMIT_AS` cap does apply; operators with unusually large Wardline REGISTRY installs see it first.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — identity translation is plugin-side; the core does not embed Wardline's qualname format. A future non-Wardline-aware plugin follows ADR-022's rules without any Wardline coupling.

## References

- [Clarion v0.1 requirements — REQ-INTEG-WARDLINE-01, NFR-COMPAT-02](../v0.1/requirements.md) (lines 599, 889) — REGISTRY pin and skew behaviour.
- [Clarion v0.1 system design §2 (Direct REGISTRY import), §9 (state-file ingest), §9 (Entity resolve oracle)](../v0.1/system-design.md) — import pattern, ingest paths, HTTP oracle.
- [Clarion v0.1 detailed design §2 (Identity reconciliation across the suite)](../v0.1/detailed-design.md) (lines 553-571) — three-scheme translation table; ingest-path rules.
- [Loom doctrine §5 (v0.1 asterisks), §6 (What Loom is NOT)](../../suite/loom.md) — initialization-coupling asterisk; "no identity reconciliation service" categorical.
- [Panel doctrine synthesis](../../implementation/v0.1-reviews/panel-2026-04-17/11-doctrine-panel-synthesis.md) — the asterisks framing originated here.
