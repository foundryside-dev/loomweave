//! Live, discovery-glob-named binary for the `loomweave-plugin-rust` wheel.
//!
//! Named `loomweave-plugin-rust` (matching the `loomweave-plugin-*` discovery
//! glob) so that, once installed in the same venv `bin/` as the `loomweave`
//! wheel, exe-dir discovery finds it and resolves its manifest from the wheel's
//! `share/loomweave/plugins/rust/plugin.toml` (install-prefix fallback). This
//! crate is OUT of the cargo workspace precisely so this glob-named artifact is
//! never produced by a dev `cargo build --workspace --bins`.
//!
//! The serve loop is the shared [`loomweave_plugin_rust::serve::run`] — identical
//! to the off-glob `loomweave-rust-plugin` dev/test binary.

fn main() {
    loomweave_plugin_rust::serve::run()
}
