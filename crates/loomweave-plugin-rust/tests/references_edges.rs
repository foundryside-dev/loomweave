//! Phase 2 — anchored `references` edges (type positions + expression paths).
//!
//! Drives the edges-aware extraction entry point with a real `Resolver` built
//! over a small in-memory project, pinning the D3 envelope row by row:
//!
//! - **IN** — type-position paths (struct/enum-variant field types, fn param +
//!   return types, type-alias RHS, const/static declared types, recursing into
//!   nested generic args) and body/initializer expression paths (`Expr::Path`
//!   not in call-callee position, `Expr::Struct` literal paths).
//! - **OUT** — `use` statements (imports owns), call callees (calls owns —
//!   args still walked), derive lists (derives owns), impl-header trait/self
//!   types, macro bodies/arguments, `Self`/`self` keyword paths.
//!
//! Resolution is kind-unfiltered (`resolve_use_path`): Resolved → `resolved`,
//! Ambiguous → `ambiguous` (never faked resolved), External → dropped and
//! counted (this absorbs no-match too — syn cannot distinguish). Self-edges
//! (`to_id == from_id`) are dropped as noise. Duplicates dedup per file on
//! `(from_id, to_id)`, first-span-wins.
//!
//! Every assertion checks the emitted `references` edge SET exactly (from, to,
//! confidence, byte span) — never just presence — because the edge PK is
//! `(kind, from_id, to_id)` and the storage writer's `ON CONFLICT` upsert
//! silently merges duplicate rows.

use loomweave_plugin_rust::extract::{Extracted, extract_file_full, extract_file_with_edges};
use loomweave_plugin_rust::resolve::Resolver;
use loomweave_plugin_rust::symbol_table::build_symbol_table;

/// Stage a one-crate project (`c_crate`) whose `lib.rs` IS `src`, build the
/// real symbol table over it, and run the edges-aware extraction of `src` as
/// the crate-root module.
fn extract_crate_root(src: &str) -> Extracted {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), src).unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);
    extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap()
}

/// The full `references` edge SET as comparable `(from, to, confidence, start,
/// end)` tuples, sorted — exact-set assertions compare against this.
fn references_set(extracted: &Extracted) -> Vec<(String, String, String, i64, i64)> {
    let mut out: Vec<_> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "references")
        .map(|e| {
            (
                e["from_id"].as_str().unwrap().to_owned(),
                e["to_id"].as_str().unwrap().to_owned(),
                e["confidence"].as_str().unwrap().to_owned(),
                e["source_byte_start"].as_i64().unwrap(),
                e["source_byte_end"].as_i64().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The Rust-populated counter triple `(sites_total, resolved_total,
/// skipped_external_total)` — exact-stats assertions compare against this.
fn stats_triple(extracted: &Extracted) -> (u64, u64, u64) {
    let s = &extracted.reference_stats;
    (s.sites_total, s.resolved_total, s.skipped_external_total)
}

/// Byte range of the `nth` (0-based) occurrence of `needle` in `src`.
fn nth_occurrence(src: &str, needle: &str, nth: usize) -> (i64, i64) {
    let start = src
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle:?} occurrence {nth} not found"))
        .0;
    (
        i64::try_from(start).unwrap(),
        i64::try_from(start + needle.len()).unwrap(),
    )
}

// Row 1: a struct field type mints a struct→type edge, span on the type token.
#[test]
fn struct_field_type_references_with_span_on_the_type_token() {
    let src = "pub struct MyType;\npub struct Holder { field: MyType }\n";
    let extracted = extract_crate_root(src);

    // Occurrence 0 is the declaration; the field TYPE token is occurrence 1.
    let (start, end) = nth_occurrence(src, "MyType", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:struct:c_crate.Holder".to_owned(),
            "rust:struct:c_crate.MyType".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "exactly one Resolved references edge Holder→MyType, anchored on the \
         field type token",
    );
    assert_eq!(stats_triple(&extracted), (1, 1, 0));
    // Belt-and-braces: an anchored edge may never be `inferred`.
    assert!(
        !extracted
            .edges
            .iter()
            .any(|e| e["kind"] == "references" && e["confidence"] == "inferred"),
        "an anchored references edge must never be `inferred`",
    );
}

// Row 2: a nested generic arg mints a site for the inner type AND the outer
// container — the container resolves External and is dropped + counted.
#[test]
fn nested_generic_arg_mints_inner_type_and_counts_container_external() {
    let src = "pub struct MyType;\npub struct Holder { field: Vec<MyType> }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "MyType", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:struct:c_crate.Holder".to_owned(),
            "rust:struct:c_crate.MyType".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "only the in-project MyType lands as an edge; Vec is dropped",
    );
    // Two sites collected (Vec + MyType); Vec is the External one.
    assert_eq!(stats_triple(&extracted), (2, 1, 1));
}

// Row 3: fn param + return types mint edges from the FN entity.
#[test]
fn fn_param_and_return_types_reference_from_the_fn_entity() {
    let src = "pub struct In;\npub struct Out;\npub fn f(_a: In) -> Out {}\n";
    let extracted = extract_crate_root(src);

    let (in_start, in_end) = nth_occurrence(src, "In", 1);
    let (out_start, out_end) = nth_occurrence(src, "Out", 1);
    assert_eq!(
        references_set(&extracted),
        vec![
            (
                "rust:function:c_crate.f".to_owned(),
                "rust:struct:c_crate.In".to_owned(),
                "resolved".to_owned(),
                in_start,
                in_end,
            ),
            (
                "rust:function:c_crate.f".to_owned(),
                "rust:struct:c_crate.Out".to_owned(),
                "resolved".to_owned(),
                out_start,
                out_end,
            ),
        ],
        "param and return types both reference from the fn entity",
    );
    assert_eq!(stats_triple(&extracted), (2, 2, 0));
}

// Row 4: an impl-method body ref originates from the METHOD entity id (the
// one with the `.impl` discriminator), not the impl or the module.
#[test]
fn impl_method_body_reference_originates_from_the_method_entity() {
    let src = "pub struct Cfg;\npub struct S;\nimpl S { pub fn go(&self) { let _ = Cfg; } }\n";
    let extracted = extract_crate_root(src);

    // Recover the method's full entity id (the same id the calls walk uses).
    let method_id = extracted
        .entities
        .iter()
        .find(|e| {
            e["kind"] == "function"
                && e["qualified_name"]
                    .as_str()
                    // The last dotted segment is the method name (NOT a
                    // file-extension check — clippy false-positives on
                    // `ends_with(".go")`).
                    .is_some_and(|q| q.contains(".impl") && q.rsplit('.').next() == Some("go"))
        })
        .and_then(|e| e["id"].as_str())
        .expect("the impl method entity")
        .to_owned();
    assert!(
        method_id.contains(".impl"),
        "method id must carry the impl discriminator, got {method_id}",
    );

    let (start, end) = nth_occurrence(src, "Cfg", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            method_id,
            "rust:struct:c_crate.Cfg".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "the body ref must originate from the METHOD entity id",
    );
}

// Row 5: a type-alias RHS references from the type_alias entity.
#[test]
fn type_alias_rhs_references_from_the_alias_entity() {
    let src = "pub struct MyType;\npub type Alias = MyType;\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "MyType", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:type_alias:c_crate.Alias".to_owned(),
            "rust:struct:c_crate.MyType".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "the alias RHS references from the type_alias entity",
    );
    assert_eq!(stats_triple(&extracted), (1, 1, 0));
}

// Row 6: a const's declared type AND its initializer path both reference from
// the const entity (two distinct targets → two edges).
#[test]
fn const_declared_type_and_initializer_path_reference_from_the_const_entity() {
    let src = "pub struct Conf;\npub const SEED: usize = 7;\npub const DEFAULT: Conf = SEED;\n";
    let extracted = extract_crate_root(src);

    // `Conf` occurrence 1 is DEFAULT's declared type; `SEED` occurrence 1 is
    // DEFAULT's initializer (occurrence 0 declares it).
    let (conf_start, conf_end) = nth_occurrence(src, "Conf", 1);
    let (seed_start, seed_end) = nth_occurrence(src, "SEED", 1);
    assert_eq!(
        references_set(&extracted),
        vec![
            (
                "rust:const:c_crate.DEFAULT".to_owned(),
                "rust:const:c_crate.SEED".to_owned(),
                "resolved".to_owned(),
                seed_start,
                seed_end,
            ),
            (
                "rust:const:c_crate.DEFAULT".to_owned(),
                "rust:struct:c_crate.Conf".to_owned(),
                "resolved".to_owned(),
                conf_start,
                conf_end,
            ),
        ],
        "declared type and initializer path both reference from the const",
    );
    // SEED's own declared type `usize` is the one External site.
    assert_eq!(stats_triple(&extracted), (3, 2, 1));
}

// Row 6 (static flavour): a static's declared type + initializer ride the same
// channel; a same-target pair dedups to ONE edge, span on the FIRST site (the
// declared type token).
#[test]
fn static_declared_type_and_initializer_reference_from_the_static_entity() {
    let src = "pub struct Conf;\npub static GLOBAL: Conf = Conf;\n";
    let extracted = extract_crate_root(src);

    // Both sites target Conf: the declared type (occurrence 1) wins the span;
    // the initializer (occurrence 2) dedups away.
    let (start, end) = nth_occurrence(src, "Conf", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:static:c_crate.GLOBAL".to_owned(),
            "rust:struct:c_crate.Conf".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "type + initializer to the same target dedup to one edge, first span",
    );
    // Both sites still COUNT (dedup is emission-side, not site-side).
    assert_eq!(stats_triple(&extracted), (2, 2, 0));
}

// Row 7: an enum-variant field type references from the ENUM entity.
#[test]
fn enum_variant_field_type_references_from_the_enum_entity() {
    let src = "pub struct Payload;\npub enum E { A(Payload), B }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "Payload", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:enum:c_crate.E".to_owned(),
            "rust:struct:c_crate.Payload".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "a variant field type references from the enum entity",
    );
    assert_eq!(stats_triple(&extracted), (1, 1, 0));
}

// Row 8: a body non-call path (`let x = LIMIT;`) mints a fn→const edge.
#[test]
fn body_non_call_path_references_a_const() {
    let src = "pub const LIMIT: usize = 8;\npub fn f() { let _x = LIMIT; }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "LIMIT", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:function:c_crate.f".to_owned(),
            "rust:const:c_crate.LIMIT".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "a non-call expression path mints a fn→const references edge",
    );
    // LIMIT's own declared type `usize` is the one External site.
    assert_eq!(stats_triple(&extracted), (2, 1, 1));
}

// Row 9: a struct literal (`Foo { a: 1 }`) in a body mints a fn→Foo edge.
#[test]
fn struct_literal_path_references_the_struct() {
    let src = "pub struct Foo { pub a: i32 }\npub fn f() { let _ = Foo { a: 1 }; }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "Foo", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:function:c_crate.f".to_owned(),
            "rust:struct:c_crate.Foo".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "a struct literal path mints a fn→struct references edge",
    );
}

// Row 10: a call CALLEE is never minted (calls owns it) — but the call's ARGS
// are still walked.
#[test]
fn call_callee_is_not_minted_but_args_are() {
    let src =
        "pub fn helper(_x: usize) {}\npub const OTHER: usize = 1;\npub fn f() { helper(OTHER); }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "OTHER", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:function:c_crate.f".to_owned(),
            "rust:const:c_crate.OTHER".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "only the call ARG references; the callee path mints no references edge",
    );
    // The callee rides the calls channel instead (in-project → a calls edge).
    assert!(
        extracted
            .edges
            .iter()
            .any(|e| e["kind"] == "calls" && e["to_id"] == "rust:function:c_crate.helper"),
        "the callee is owned by the calls channel",
    );
    assert!(
        !extracted
            .edges
            .iter()
            .any(|e| e["kind"] == "references" && e["to_id"] == "rust:function:c_crate.helper"),
        "the callee must NOT also be a references edge",
    );
}

// Row 11: a method-call RECEIVER path IS minted (`CONFIG.get()` → fn→CONFIG).
#[test]
fn method_call_receiver_path_is_minted() {
    let src = "pub static CONFIG: usize = 0;\npub fn f() { CONFIG.get(); }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "CONFIG", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:function:c_crate.f".to_owned(),
            "rust:static:c_crate.CONFIG".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "a method-call receiver path references normally",
    );
}

// Row 12: `use` statements mint NO references (imports owns them).
#[test]
fn use_statements_mint_no_references() {
    let src = "pub mod m { pub struct S; }\nuse crate::m::S;\n";
    let extracted = extract_crate_root(src);

    assert_eq!(
        references_set(&extracted),
        Vec::new(),
        "`use` paths belong to the imports channel, never references",
    );
    assert_eq!(stats_triple(&extracted), (0, 0, 0));
    // Non-vacuous: the use DID resolve — as an imports edge.
    assert!(
        extracted
            .edges
            .iter()
            .any(|e| e["kind"] == "imports" && e["to_id"] == "rust:struct:c_crate.m.S"),
        "the use statement resolves on the imports channel",
    );
}

// Row 13: derive lists and impl headers (trait path + self type) mint nothing.
#[test]
fn derive_list_and_impl_header_mint_no_references() {
    let src = "pub trait Pretty {}\n#[derive(Pretty)]\npub struct Foo;\nimpl Pretty for Foo {}\n";
    let extracted = extract_crate_root(src);

    assert_eq!(
        references_set(&extracted),
        Vec::new(),
        "derive lists (derives owns) and impl headers (implements owns) mint \
         no references",
    );
    assert_eq!(stats_triple(&extracted), (0, 0, 0));
    // Non-vacuous: both paths resolved on their OWN channels.
    assert!(
        extracted.edges.iter().any(|e| e["kind"] == "derives"),
        "the derive list rides the derives channel",
    );
    assert!(
        extracted.edges.iter().any(|e| e["kind"] == "implements"),
        "the impl header rides the implements channel",
    );
}

// Row 14: external / primitive types mint no edge and count External.
#[test]
fn external_and_primitive_types_count_external() {
    let src = "pub struct H { a: u32, b: String }\n";
    let extracted = extract_crate_root(src);

    assert_eq!(
        references_set(&extracted),
        Vec::new(),
        "primitives / external types must yield ZERO references edges",
    );
    assert_eq!(stats_triple(&extracted), (2, 0, 2));
}

// Row 15: macro arguments mint NOTHING (spec §5 — macro bodies are opaque).
#[test]
fn macro_arguments_mint_nothing() {
    let src = "pub struct MyType;\npub fn f() { println!(\"{}\", MyType::X); }\n";
    let extracted = extract_crate_root(src);

    assert_eq!(
        references_set(&extracted),
        Vec::new(),
        "macro arguments are opaque tokens — no references sites",
    );
    assert_eq!(stats_triple(&extracted), (0, 0, 0));
}

// Row 16: the same target referenced twice in one fn dedups to ONE edge,
// span = the FIRST site.
#[test]
fn duplicate_references_dedup_to_one_edge_with_the_first_span() {
    let src = "pub struct Twice;\npub fn f() { let _a = Twice; let _b = Twice; }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "Twice", 1);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:function:c_crate.f".to_owned(),
            "rust:struct:c_crate.Twice".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "duplicate (from, to) pairs dedup to one edge, first-span-wins",
    );
    // Both sites still COUNT (dedup is emission-side, not site-side).
    assert_eq!(stats_triple(&extracted), (2, 2, 0));
}

// Row 17: a self-reference (a type naming itself in its own fields) is
// dropped; the wrapping containers count External.
#[test]
fn self_reference_is_dropped() {
    let src = "pub struct Foo { next: Option<Box<Foo>> }\n";
    let extracted = extract_crate_root(src);

    assert_eq!(
        references_set(&extracted),
        Vec::new(),
        "Foo→Foo is noise and must be dropped; Option/Box are External",
    );
    // Three sites: Option (External), Box (External), Foo (Resolved — counted
    // resolved even though the self-edge is not emitted).
    assert_eq!(stats_triple(&extracted), (3, 1, 2));
}

// Row 18: exact counter triple over one mixed file.
#[test]
fn stats_triple_is_exact_over_a_mixed_file() {
    // Sites in source order:
    //   H.x: A        → Resolved
    //   H.y: u32      → External
    //   f param A     → Resolved
    //   f return Vec  → External
    //   f return A    → Resolved (dedups against the param site at emit)
    //   f body q      → External (no match — absorbed by the external counter)
    let src = "pub struct A;\npub struct H { x: A, y: u32 }\npub fn f(_p: A) -> Vec<A> { q }\n";
    let extracted = extract_crate_root(src);

    assert_eq!(stats_triple(&extracted), (6, 3, 3));
    let (h_start, h_end) = nth_occurrence(src, "A", 1);
    let (p_start, p_end) = nth_occurrence(src, "A", 2);
    assert_eq!(
        references_set(&extracted),
        vec![
            (
                "rust:function:c_crate.f".to_owned(),
                "rust:struct:c_crate.A".to_owned(),
                "resolved".to_owned(),
                p_start,
                p_end,
            ),
            (
                "rust:struct:c_crate.H".to_owned(),
                "rust:struct:c_crate.A".to_owned(),
                "resolved".to_owned(),
                h_start,
                h_end,
            ),
        ],
        "the edge SET dedups f's two A sites to the first (param) span",
    );
}

// Row 19: a multi-KIND qualname collision lands `ambiguous`, never a faked
// `resolved`. `struct Same` + `fn Same` share the qualname `c_crate.Same`
// under two kinds; the kind-unfiltered resolver returns Ambiguous with the
// first candidate by sorted id order (deterministic).
#[test]
fn multi_kind_collision_is_ambiguous_never_faked_resolved() {
    let src = "pub struct Same;\npub fn Same() {}\npub fn user() { let _ = Same; }\n";
    let extracted = extract_crate_root(src);

    // Occurrences of `Same`: 0 = struct decl, 1 = fn decl, 2 = the body ref.
    let (start, end) = nth_occurrence(src, "Same", 2);
    assert_eq!(
        references_set(&extracted),
        vec![(
            "rust:function:c_crate.user".to_owned(),
            // First candidate by sorted order: "function" < "struct".
            "rust:function:c_crate.Same".to_owned(),
            "ambiguous".to_owned(),
            start,
            end,
        )],
        "a multi-kind collision must land ambiguous with the first-sorted \
         candidate, never a guessed resolved",
    );
    // Ambiguous counts toward resolved_total (≥1 in-project candidate).
    assert_eq!(stats_triple(&extracted), (1, 1, 0));
}

// Parity guard: the no-resolver entry point emits NO references edges and
// zero counters — exactly like imports/calls/derives.
#[test]
fn no_resolver_path_emits_no_references_and_zero_stats() {
    let src = "pub struct MyType;\npub struct H { f: MyType }\n";
    let extracted = extract_file_full("c_crate", "c_crate", "/p/c/src/lib.rs", src).unwrap();
    assert_eq!(
        references_set(&extracted),
        Vec::new(),
        "no-resolver path must emit zero references edges",
    );
    assert_eq!(stats_triple(&extracted), (0, 0, 0));
    // Non-vacuous: entities still extracted.
    assert!(extracted.entities.iter().any(|e| e["kind"] == "struct"));
}
