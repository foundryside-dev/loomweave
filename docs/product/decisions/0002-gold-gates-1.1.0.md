# PDR-0002: Gold gates the 1.1.0 cut

- **Date:** 2026-06-11
- **Status:** accepted (owner decision, confirmed via bootstrap Q&A)

## Context

Sprint-4's Rust-plugin closeout verdict was **not gold**: four entity-ID
collision families (self-type-path, trait-path, `#[path]`-module, `const _`)
were found and filed as blockers. The alternative was to cut 1.1.0 with the
families documented as known limitations.

## The call

1.1.0 does not cut from rc4 until all four collision families are fixed and
a gold verdict is recorded. Identity correctness is the product's core
promise (SEI, stable graph); shipping known identity collisions undercuts it.

## Reversal trigger

If the collision-family fixes are still open by **2026-06-30** (the
north-star placeholder date), re-raise the gate question with the owner
rather than letting rc4 drift dateless.
