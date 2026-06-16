# Weft suite glossary

> **This glossary has been promoted to the Weft federation hub.**
> The canonical, authoritative cross-product vocabulary catalogue — every term
> whose meaning crosses product boundaries, with its managed/renamed/no-clash
> verdict and authority — now lives at
> **`~/loom/glossary.md`** (authoritative as of 2026-06-05).
> This file is retained as a **pointer stub** so existing references resolve.

---

The body of this file (the how-to-use guidance, the ADR-acceptance rule, the
status legend, and the cross-product term tables — managed clashes, renamed
clashes, no-clash informational entries, the SP9 Wardline taint-store wire terms,
deferred clashes, Wardline-side terms, and the Shuttle note) is now authoritative
at `~/loom/glossary.md`. Read and update it
there.

The federation axiom this glossary defends is the cross-product field-name rule
in the hub doctrine: `~/loom/doctrine.md` §8.

Loomweave's ADRs (e.g. ADR-004, ADR-017, ADR-022, ADR-024, ADR-036, ADR-038)
remain Loomweave-owned and authoritative for Loomweave's own field shapes; the hub
glossary points to them, not the reverse.

---

## Managed clashes (mirror to the hub)

The body of cross-product term tables now lives at `~/loom/glossary.md`. New
managed-clash verdicts are recorded here as well so the in-repo ADR-acceptance
gate (`docs/loomweave/adr/README.md` §"ADR acceptance criteria") resolves without
the hub; the hub copy is canonical and should mirror this entry.

| Term | Verdict | Authority | Mapping / notes |
|---|---|---|---|
| `ephemeral.port` (read-API live-port discovery file) | **managed clash** | ADR-044 (Loomweave); Filigree owns the original `.filigree/ephemeral.port` convention | Shared filename convention, **distinct per-product paths**: `.filigree/ephemeral.port` ↔ `.loomweave/ephemeral.port`. Identical format (single plain-ASCII TCP port, optional trailing `\n`, atomic temp+rename), loopback-only publication, present only while the producer serves. Bands are disjoint and never part of the contract — consumers read the file, never recompute. Mapping table in ADR-044 §"Managed-clash verdict". |
