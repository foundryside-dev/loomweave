# Federation Docs

Loomweave's sibling-facing federation material: the normative contract that
Filigree, Wardline, and Warpline consume, plus the dated change-sets and
responses exchanged with those products while the contract was settled.

| Path | Role | Contents |
|---|---|---|
| [contracts.md](./contracts.md) | Normative | Loomweave federation contracts — read-side HTTP surface and exchange types. |
| [sei-migration-playbook.md](./sei-migration-playbook.md) | Normative | SEI hard-cutover migration playbook. |
| [2026-07-12-federation-seam-golden-authority.md](./2026-07-12-federation-seam-golden-authority.md) | Normative | Federation seam golden authority — which fixtures are authoritative and how they are verified. |
| [fixtures/](./fixtures/) | Normative | Golden JSON fixtures and `.sha256` sidecars for the federation seam (capabilities, files, identity ownership, HTTP auth, SEI conformance, Warpline vectors, classification). |
| [2026-05-30-loomweave-wardline-taint-store-response.md](./2026-05-30-loomweave-wardline-taint-store-response.md) | Dated response | Loomweave → Wardline: taint-store contract response (SP9). |
| [2026-05-30-prune-unseen-filigree-request.md](./2026-05-30-prune-unseen-filigree-request.md) | Dated request | Request to Filigree for a `scan_source`-scoped prune surface (withdrawn/superseded the same day — see its header). |
| [2026-06-09-rust-qualname-dialect-response.md](./2026-06-09-rust-qualname-dialect-response.md) | Dated response | Loomweave → Wardline: Rust qualname dialect resolved and pinned. |
| [2026-06-09-rust-qualname-phase1b-changeset.md](./2026-06-09-rust-qualname-phase1b-changeset.md) | Dated change-set | Loomweave → Wardline: Rust qualname dialect Phase 1b conformance change-set. |
| [2026-06-11-rust-qualname-amendment-4-5-changeset.md](./2026-06-11-rust-qualname-amendment-4-5-changeset.md) | Dated change-set | Loomweave → Wardline: Rust qualname dialect Amendments 4 & 5 change-set / handoff. |
| [2026-06-11-rust-qualname-amendment-6-9-changeset.md](./2026-06-11-rust-qualname-amendment-6-9-changeset.md) | Dated change-set | Loomweave → Wardline: Rust qualname dialect Amendments 6–9 change-set / handoff. |
| [filigree-side/](./filigree-side/README.md) | Mirror | Read-only copies of Filigree-authored planning artifacts (ADR-014 registry backend, cross-project sequencing memo) kept for local cross-reference. |

The Rust qualname dialect itself is frozen in
[ADR-049](../loomweave/adr/ADR-049-rust-qualname-canonicalization.md); the dated change-sets above record how
Wardline was brought into conformance with it.
