# Rust Plugin Gold Closeout — v2 addendum (2026-06-11)

Non-normative QA memo, addendum to `2026-06-11-rust-plugin-gold-qa.md` (the v1
report). Branch rc4, HEAD `4de391e`. Same harness, same pinned corpus SHAs,
same `qualname_check` collision oracle.

## The four filed families are closed

All four gold-blocker families the v1 report filed are fixed on rc4, each via
its own ADR-049 amendment:

| family | ticket | amendment | fix | commit |
|---|---|---|---|---|
| self-type last-segment | `clarion-8ff7f233fa` | 6 | twin-gated written-path qualification of the `<Type>` base (ladder stage S) | `c4791aa` |
| trait-path last-segment | `clarion-fa8bcf8731` | 7 | twin-gated written-path qualification of the `impl[…]` fragment (ladder stage T) | `c4791aa` |
| `#[path]` module alias | `clarion-bdb1eccf48` | 8 | init-time mount overlay with filesystem default; cross-form `@cfg` twin rule | `05b44f3` |
| unnamed `const _` | `clarion-83870dc534` | 9 | `const _` is not an entity — unconditional skip-emission | `f7f8a69` |

`4de391e` is the adversarial-review hardening pass over all four (below).

## Per-corpus final numbers (pinned SHAs, post-`4de391e` re-sweep)

| corpus | commit | entities (was v1) | collisions (was v1) |
|---|---|---|---|
| ripgrep | `82313cf9` | 2 169 (2 169) | **0** (0) |
| serde | `5f0f18b9` | 1 674 (1 673) | **0** (0) |
| tokio | `2e7930fe` | 7 788 (7 771) | **0** (was 12) |
| rust-analyzer | `587ce15e` | 31 357 (31 376) | **0** (was 15) |

Entity counts shifted vs the v1 table for three expected reasons: **`#[path]`
subtree re-keying** (Amendment 8 — mounted trees now route to their mount ids,
so a previously-aliased facade/mounted pair mints distinct module entities and
whole subtrees re-key), **`const _` removals** (Amendment 9 — every anonymous
const entity, colliding or lone, leaves the emitted set; the bulk of
rust-analyzer's −19), and **new split impls** (Amendments 6–7 — an S/T-fired
group un-merges what was one chimera impl entity into its real members, plus
their methods). SEI churn on an unchanged re-analyze remains minted=0
orphaned=0 on all four.

## Adversarial-review hardening (pre-handoff)

A three-lens review of `f7f8a69`/`c4791aa`/`05b44f3` found and fixed, before
any handoff: one **new collision family** — a `#[path]` mount declared inside a
cfg-twin inline mod composed the *bare* inline prefix, routing both twins'
targets to one module id; mount prefixes now compose the inline mod's twin
`@cfg` segment, mirroring the AST walk byte-for-byte (corpus row
`path_mount_inside_cfg_twin_inline_mod`) — and a **leading-`::` witness
asymmetry** — stage S ignored a leading `::` that stage T honored, so
`::a::X` vs `a::X` self-type twins silently chimera'd; the S witness and
qualified render now carry it symmetrically (`self_type_path_leading_colon_twin`).
Plus ladder-coverage rows/tests and ADR-text reconciliation (`4de391e`). The
corpus oracle was re-swept after the hardening: still 0/0/0/0.

## Revised gold verdict

The v1 verdict's **single named not-gold reason — real-corpus qualname
collisions are not zero — is cleared**: 27 → 0 across all four corpora at the
pinned SHAs, with conformance, SEI stability, and the full CI floor green
(1 643 workspace tests).

**Gold for the producer pair is still pending Wardline lockstep.** Remaining:

1. **Wardline re-vendor of the batched 4–9 change-set** — escalation-gated at
   their rc5 branch; handoff letter
   `docs/federation/2026-06-11-rust-qualname-amendment-6-9-changeset.md`
   (corpus 49 entity rows + 8 mount rows, md5
   `a784a2f97e2079c71b7aba87c11694dd`). Until both producers emit the amended
   dialect byte-for-byte, the pair is not gold.
2. **Duplicate-id runtime visibility** (`clarion-b19fe90c3e`) stands as the
   standing alarm: the analyze path still emits no finding on a duplicate id —
   only the `dogfood_uniqueness` test and the `qualname_check` oracle consult
   `duplicate_ids()` — so a future fifth family would again be silent in
   production until swept.
