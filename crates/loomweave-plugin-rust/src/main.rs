//! Rust language plugin binary — Phase 1a identity foundation.
//!
//! The cargo artifact is named `loomweave-rust-plugin`, deliberately OFF the
//! `loomweave-plugin-*` discovery glob (see `loomweave-core/src/plugin/
//! discovery.rs`): a `cargo build --workspace --bins` drops it next to
//! `loomweave`, and a glob-matching name would make `current_exe()`-relative
//! discovery find a manifest-less "plugin" beside the running binary and fail
//! the run. The host's automated tests self-stage it under the discovery name.
//! The LIVE, glob-named artifact is built by the out-of-workspace distribution
//! crate (`packaging/rust-plugin-dist`) that maturin packages into the
//! `loomweave-plugin-rust` wheel — both bins share the single entry point
//! [`loomweave_plugin_rust::serve::run`] so they cannot drift.

fn main() {
    loomweave_plugin_rust::serve::run()
}
