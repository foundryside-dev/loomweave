//! Plugin manifest parser and validator.
//!
//! Implements the L5 `plugin.toml` schema per ADR-022 and ADR-021 §Layer 1.
//!
//! # Usage
//!
//! ```no_run
//! use clarion_core::plugin::parse_manifest;
//!
//! let bytes = std::fs::read("plugin.toml").unwrap();
//! let manifest = parse_manifest(&bytes).unwrap();
//! ```
//!
//! After parsing, call [`Manifest::validate_for_v0_1`] to run the
//! ADR-021 §Layer 1 capability checks that the supervisor (Task 6) needs
//! before completing the `initialize` handshake.

use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

use crate::entity_id::validate_kind_grammar;

// ── Reserved lists (ADR-022) ──────────────────────────────────────────────────

/// Entity kinds the core owns; plugins may not declare these (ADR-022 §Core owns).
const RESERVED_ENTITY_KINDS: &[&str] = &["file", "subsystem", "guidance"];

/// Rule-ID prefixes the core owns; plugins may not claim these (ADR-022 §Core owns).
///
/// `CLA-INFRA-` is core/pipeline-only; `CLA-FACT-` is shared (core or any tool may
/// emit) but a plugin manifest may not claim it as *the plugin's* prefix.
const RESERVED_RULE_ID_PREFIXES: &[&str] = &["CLA-INFRA-", "CLA-FACT-"];

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by [`parse_manifest`] and [`Manifest::validate_for_v0_1`].
///
/// Each variant corresponds to a `CLA-INFRA-MANIFEST-*` finding code that Task 6
/// surfaces in the `initialize` handshake reply. Use [`ManifestError::subcode`] to
/// obtain the machine-readable finding code.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    /// TOML parse failure or a required field is absent.
    ///
    /// Finding code: `CLA-INFRA-MANIFEST-MALFORMED`.
    #[error("CLA-INFRA-MANIFEST-MALFORMED: {message}")]
    Malformed { message: String },

    /// An identifier string fails the ADR-022 grammar `[a-z][a-z0-9_]*` (kinds)
    /// or `CLA-[A-Z]+(-[A-Z0-9]+)*-` (rule-ID prefix).
    ///
    /// Finding code: `CLA-INFRA-MANIFEST-MALFORMED`.
    #[error("CLA-INFRA-MANIFEST-MALFORMED: {field} {value:?} violates ADR-022 identifier grammar")]
    GrammarViolation { field: &'static str, value: String },

    /// A plugin manifest declares one of the core-reserved entity kinds.
    ///
    /// Finding code: `CLA-INFRA-MANIFEST-RESERVED-KIND`.
    #[error(
        "CLA-INFRA-MANIFEST-RESERVED-KIND: entity kind {kind:?} is reserved by the core (ADR-022)"
    )]
    ReservedKind { kind: String },

    /// A plugin manifest claims a rule-ID prefix owned by the core.
    ///
    /// Finding code: `CLA-INFRA-RULE-ID-NAMESPACE`.
    #[error(
        "CLA-INFRA-RULE-ID-NAMESPACE: rule_id_prefix {prefix:?} is a core-reserved namespace (ADR-022)"
    )]
    ReservedPrefix { prefix: String },

    /// A manifest declares a capability that v0.1 does not support.
    ///
    /// Finding code: `CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY`.
    ///
    /// This variant is produced by [`Manifest::validate_for_v0_1`], not by
    /// [`parse_manifest`]. The parser accepts the field faithfully; Task 6's
    /// supervisor calls `validate_for_v0_1` and surfaces this error as a
    /// handshake rejection.
    #[error(
        "CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY: capability {capability:?} is not supported in v0.1"
    )]
    UnsupportedCapability { capability: &'static str },
}

impl ManifestError {
    /// Return the machine-readable finding code for this error.
    ///
    /// Task 6 uses this to populate the `rule_id` field of the `CLA-INFRA-*`
    /// finding emitted when a plugin fails to start.
    pub fn subcode(&self) -> &'static str {
        match self {
            ManifestError::Malformed { .. } | ManifestError::GrammarViolation { .. } => {
                "CLA-INFRA-MANIFEST-MALFORMED"
            }
            ManifestError::ReservedKind { .. } => "CLA-INFRA-MANIFEST-RESERVED-KIND",
            ManifestError::ReservedPrefix { .. } => "CLA-INFRA-RULE-ID-NAMESPACE",
            ManifestError::UnsupportedCapability { .. } => {
                "CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY"
            }
        }
    }
}

// ── Manifest structs ──────────────────────────────────────────────────────────

/// Top-level `plugin.toml` manifest.
///
/// Serde deserialises from TOML. `#[serde(deny_unknown_fields)]` is intentionally
/// absent at the top level so that future `[integrations.*]` blocks (WP3) parse
/// without error.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Manifest {
    /// `[plugin]` table.
    pub plugin: PluginMeta,

    /// `[capabilities]` table.
    pub capabilities: Capabilities,

    /// `[ontology]` table.
    pub ontology: Ontology,

    /// `[integrations.*]` — optional, opaque passthrough for plugin-specific
    /// integration config (e.g. WP3's `[integrations.wardline]`).
    ///
    /// The core does not interpret this table; it is preserved so Task 6 can
    /// forward it to the plugin during `initialize` if needed.
    ///
    /// Entry count is capped at [`MAX_INTEGRATIONS`] at parse time — see
    /// [`parse_manifest`].
    #[serde(default)]
    pub integrations: BTreeMap<String, toml::Value>,
}

/// Maximum number of `[integrations.*]` entries accepted per manifest.
///
/// A typical plugin has 0–1 integration blocks (WP3 adds
/// `[integrations.wardline]`); 64 is an order-of-magnitude ceiling that
/// covers any legitimate future use while rejecting pathologies.
pub const MAX_INTEGRATIONS: usize = 64;

/// `[plugin]` metadata table.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMeta {
    /// Package name, e.g. `"clarion-plugin-python"`. Informational; hyphens allowed.
    pub name: String,

    /// Identifier fed to `entity_id()`, e.g. `"python"`. Must satisfy `[a-z][a-z0-9_]*`
    /// per ADR-022. Distinct from `name` so human-readable package names (which may
    /// contain hyphens) do not conflict with the entity-ID grammar.
    pub plugin_id: String,

    /// Plugin version (semver), e.g. `"0.1.0"`.
    pub version: String,

    /// Protocol version the plugin speaks, e.g. `"1.0"`.
    pub protocol_version: String,

    /// Executable name (resolved via `$PATH` or neighboring manifest per L9).
    pub executable: String,

    /// Informational language tag.
    pub language: String,

    /// File extensions this plugin claims, e.g. `["py"]`.
    pub extensions: Vec<String>,
}

/// `[capabilities]` table — wraps the ADR-021 §Layer 1 runtime sub-struct.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// `[capabilities.runtime]` — ADR-021 §Layer 1 declarations.
    pub runtime: CapabilitiesRuntime,
}

/// `[capabilities.runtime]` — ADR-021 §Layer 1 declarations.
///
/// These are *declarations*, not enforcements. The core applies its own
/// absolute ceilings independently (L6, Task 4); effective limits are
/// `min(manifest, core_default)`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesRuntime {
    /// Plugin's own RSS estimate in MiB. Effective `prlimit` = `min(this, 2 GiB)`.
    ///
    /// Must be > 0.
    pub expected_max_rss_mb: u64,

    /// Declared per-file entity budget. Exceeding triggers `CLA-INFRA-PLUGIN-ENTITY-OVERRUN-WARNING`
    /// (implementation deferred to Tier B sprint).
    pub expected_entities_per_file: u64,

    /// `true` if this plugin reads `wardline.core.registry.REGISTRY` (WP3 L8).
    pub wardline_aware: bool,

    /// `true` if the plugin needs to read paths outside the project root.
    ///
    /// v0.1 refuses `true` at `initialize` with `CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY`.
    /// The parser accepts the field faithfully; [`Manifest::validate_for_v0_1`] performs
    /// the rejection check.
    pub reads_outside_project_root: bool,

    /// Optional `[capabilities.runtime.pyright]` declaration for Python call resolution.
    #[serde(default)]
    pub pyright: Option<PyrightRuntime>,
}

/// `[capabilities.runtime.pyright]` — pinned Pyright runtime metadata.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PyrightRuntime {
    /// Exact Pyright package pin used by the plugin runtime.
    pub pin: String,
}

/// `[ontology]` table — plugin-declared ontology per ADR-022.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ontology {
    /// Entity kinds this plugin emits. Must be non-empty; each must satisfy
    /// `[a-z][a-z0-9_]*`; none may be in the core-reserved list.
    pub entity_kinds: Vec<String>,

    /// Edge kinds this plugin emits. May include the four core-reserved edge kinds
    /// (`contains`, `guides`, `emits_finding`, `in_subsystem`) — listing them binds
    /// the plugin to the core's fixed semantics for those kinds (ADR-022 §Core owns).
    #[serde(default)]
    pub edge_kinds: Vec<String>,

    /// Rule-ID prefix, e.g. `"CLA-PY-"`. Must end with `-` and match
    /// `CLA-[A-Z]+(-[A-Z0-9]+)*-`. Must not be a core-reserved prefix.
    pub rule_id_prefix: String,

    /// Ontology version (semver). Bumped when entity/edge/rule set changes.
    /// WP6 includes this in the cache key (ADR-007).
    pub ontology_version: String,
}

// ── Public API ────────────────────────────────────────────────────────────────

impl Manifest {
    /// Run ADR-021 §Layer 1 capability checks.
    ///
    /// Called by Task 6's supervisor before sending `initialized` to ensure no
    /// v0.1-unsupported capability is granted. Returns `Ok(())` if the manifest
    /// is safe to proceed with, or a [`ManifestError::UnsupportedCapability`] if
    /// a capability the core cannot honour is declared.
    ///
    /// Note: [`parse_manifest`] already validates grammar and reserved names. This
    /// method only checks runtime capabilities that the core cannot satisfy.
    pub fn validate_for_v0_1(&self) -> Result<(), ManifestError> {
        if self.capabilities.runtime.reads_outside_project_root {
            return Err(ManifestError::UnsupportedCapability {
                capability: "reads_outside_project_root",
            });
        }
        Ok(())
    }
}

/// Parse and validate a `plugin.toml` manifest from raw bytes.
///
/// Performs:
/// 1. TOML deserialisation into [`Manifest`].
/// 2. Structural checks (`name` non-empty, `extensions` non-empty, etc.).
/// 3. `entity_kinds` non-empty; each matches `[a-z][a-z0-9_]*`; none in reserved list.
/// 4. `edge_kinds` each matches `[a-z][a-z0-9_]*` (core-reserved edge kinds are allowed).
/// 5. `rule_id_prefix` grammar check, then reserved-prefix check.
/// 6. `expected_max_rss_mb > 0`.
///
/// Does **not** check `reads_outside_project_root` — that is a v0.1 capability
/// restriction surfaced by [`Manifest::validate_for_v0_1`] at handshake time.
///
/// # Errors
///
/// Returns a [`ManifestError`] describing the first validation failure.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    // 1. TOML deserialise.
    let text = std::str::from_utf8(bytes).map_err(|e| ManifestError::Malformed {
        message: format!("manifest is not valid UTF-8: {e}"),
    })?;
    let manifest: Manifest = toml::from_str(text).map_err(|e| ManifestError::Malformed {
        message: e.to_string(),
    })?;

    // Cap the integrations table size. WP3 expects one entry
    // (`[integrations.wardline]`); real-world plugin.toml files have at
    // most a handful. MAX_INTEGRATIONS is a trust-boundary cap — an
    // attacker-controlled plugin.toml with millions of `[integrations.*]`
    // entries would otherwise live in memory for the run lifetime.
    if manifest.integrations.len() > MAX_INTEGRATIONS {
        return Err(ManifestError::Malformed {
            message: format!(
                "[integrations] has {} entries; maximum is {MAX_INTEGRATIONS}",
                manifest.integrations.len(),
            ),
        });
    }

    // 2. Structural checks.
    if manifest.plugin.name.is_empty() {
        return Err(ManifestError::Malformed {
            message: "[plugin].name must not be empty".to_owned(),
        });
    }
    // plugin_id must satisfy the ADR-022 kind grammar [a-z][a-z0-9_]*.
    if manifest.plugin.plugin_id.is_empty() {
        return Err(ManifestError::Malformed {
            message: "[plugin].plugin_id must not be empty".to_owned(),
        });
    }
    if !validate_kind_grammar(&manifest.plugin.plugin_id) {
        return Err(ManifestError::GrammarViolation {
            field: "plugin_id",
            value: manifest.plugin.plugin_id.clone(),
        });
    }
    if manifest.plugin.extensions.is_empty() {
        return Err(ManifestError::Malformed {
            message: "[plugin].extensions must not be empty".to_owned(),
        });
    }
    // Extension format: lowercase ASCII alphanumeric, no dot, at least 1 char.
    // Grammar: [a-z][a-z0-9]*  (matches what Path::extension() returns — no leading dot).
    for ext in &manifest.plugin.extensions {
        if ext.is_empty()
            || !ext.starts_with(|c: char| c.is_ascii_lowercase())
            || !ext
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(ManifestError::GrammarViolation {
                field: "extensions",
                value: ext.clone(),
            });
        }
    }

    // 3. entity_kinds non-empty; grammar; reserved check.
    if manifest.ontology.entity_kinds.is_empty() {
        return Err(ManifestError::Malformed {
            message: "[ontology].entity_kinds must declare at least one kind".to_owned(),
        });
    }
    for kind in &manifest.ontology.entity_kinds {
        validate_kind_string("entity_kinds", kind)?;
        if RESERVED_ENTITY_KINDS.iter().any(|r| *r == kind) {
            return Err(ManifestError::ReservedKind {
                kind: kind.to_owned(),
            });
        }
    }

    // 4. edge_kinds grammar (core-reserved names are permitted — they bind the
    //    plugin to core semantics per ADR-022, not redefine them).
    for kind in &manifest.ontology.edge_kinds {
        validate_kind_string("edge_kinds", kind)?;
    }

    // 5. rule_id_prefix grammar then reserved check.
    validate_rule_id_prefix_grammar(&manifest.ontology.rule_id_prefix)?;
    if RESERVED_RULE_ID_PREFIXES
        .iter()
        .any(|r| *r == manifest.ontology.rule_id_prefix)
    {
        return Err(ManifestError::ReservedPrefix {
            prefix: manifest.ontology.rule_id_prefix.clone(),
        });
    }

    // 6. RSS bound.
    if manifest.capabilities.runtime.expected_max_rss_mb == 0 {
        return Err(ManifestError::Malformed {
            message: "[capabilities.runtime].expected_max_rss_mb must be > 0".to_owned(),
        });
    }

    Ok(manifest)
}

// ── Grammar helpers ───────────────────────────────────────────────────────────

/// Validate a kind string against the ADR-022 grammar `[a-z][a-z0-9_]*`.
///
/// Reuses [`validate_kind_grammar`] from `entity_id` — single canonical check.
fn validate_kind_string(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if !validate_kind_grammar(value) {
        return Err(ManifestError::GrammarViolation {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Validate a `rule_id_prefix` against the ADR-022 prefix grammar.
///
/// Rules:
/// 1. Must end with `-`.
/// 2. Strip the trailing `-`; the remainder must match `CLA-[A-Z]+(-[A-Z0-9]+)*`.
///    Implementation: split on `-`, verify the first segment is `CLA`, and each
///    subsequent non-empty segment is `[A-Z0-9]+` (ASCII uppercase or digit).
///    There must be at least one segment after `CLA` (so `CLA-` alone is invalid).
///
/// Examples of valid prefixes: `CLA-PY-`, `CLA-JAVA-`, `CLA-FOO-BAR-`.
/// Examples of invalid prefixes: `PY-`, `cla-py-`, `CLA-py-`, `CLA-PY` (no trailing
/// hyphen), `CLA-` (no segment after CLA), `CLA--PY-` (empty segment).
fn validate_rule_id_prefix_grammar(prefix: &str) -> Result<(), ManifestError> {
    // Rule 1: must end with `-`.
    let Some(without_trailing) = prefix.strip_suffix('-') else {
        return Err(ManifestError::GrammarViolation {
            field: "rule_id_prefix",
            value: prefix.to_owned(),
        });
    };

    // Rule 2: split on `-`; first segment must be `CLA`; all subsequent segments
    // must be non-empty `[A-Z0-9]+`; at least one segment must follow `CLA`.
    let segments: Vec<&str> = without_trailing.split('-').collect();

    // First segment must be exactly `CLA`.
    if segments.first().copied() != Some("CLA") {
        return Err(ManifestError::GrammarViolation {
            field: "rule_id_prefix",
            value: prefix.to_owned(),
        });
    }

    // There must be at least one segment after `CLA`.
    if segments.len() < 2 {
        return Err(ManifestError::GrammarViolation {
            field: "rule_id_prefix",
            value: prefix.to_owned(),
        });
    }

    // Remaining segments must be non-empty `[A-Z0-9]+`.
    for seg in &segments[1..] {
        if seg.is_empty()
            || !seg
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Err(ManifestError::GrammarViolation {
                field: "rule_id_prefix",
                value: prefix.to_owned(),
            });
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fixtures ──────────────────────────────────────────────────────────────

    /// The canonical valid manifest fixture (mirrors the L5 schema in §2).
    const VALID_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-python"
plugin_id = "mockplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-python"
language = "python"
extensions = ["py"]

[capabilities.runtime]
expected_max_rss_mb = 512
expected_entities_per_file = 5000
wardline_aware = true
reads_outside_project_root = false

[capabilities.runtime.pyright]
pin = "1.1.409"

[ontology]
entity_kinds = ["function", "class", "module", "decorator"]
edge_kinds = ["imports", "calls", "decorates", "contains"]
rule_id_prefix = "CLA-PY-"
ontology_version = "0.1.0"
"#;

    // ── Positive: full parse ──────────────────────────────────────────────────

    #[test]
    fn positive_parse_valid_manifest_all_fields_populated() {
        let manifest = parse_manifest(VALID_MANIFEST.as_bytes()).unwrap();

        // [plugin]
        assert_eq!(manifest.plugin.name, "clarion-plugin-python");
        assert_eq!(manifest.plugin.plugin_id, "mockplugin");
        assert_eq!(manifest.plugin.version, "0.1.0");
        assert_eq!(manifest.plugin.protocol_version, "1.0");
        assert_eq!(manifest.plugin.executable, "clarion-plugin-python");
        assert_eq!(manifest.plugin.language, "python");
        assert_eq!(manifest.plugin.extensions, vec!["py"]);

        // [capabilities.runtime]
        assert_eq!(manifest.capabilities.runtime.expected_max_rss_mb, 512);
        assert_eq!(
            manifest.capabilities.runtime.expected_entities_per_file,
            5000
        );
        assert!(manifest.capabilities.runtime.wardline_aware);
        assert!(!manifest.capabilities.runtime.reads_outside_project_root);
        assert_eq!(
            manifest
                .capabilities
                .runtime
                .pyright
                .as_ref()
                .map(|pyright| pyright.pin.as_str()),
            Some("1.1.409")
        );

        // [ontology]
        assert_eq!(
            manifest.ontology.entity_kinds,
            vec!["function", "class", "module", "decorator"]
        );
        assert_eq!(
            manifest.ontology.edge_kinds,
            vec!["imports", "calls", "decorates", "contains"]
        );
        assert_eq!(manifest.ontology.rule_id_prefix, "CLA-PY-");
        assert_eq!(manifest.ontology.ontology_version, "0.1.0");
    }

    // ── Positive: core-reserved edge kind allowed in edge_kinds ──────────────

    #[test]
    fn positive_core_reserved_edge_kind_in_edge_kinds_parses_successfully() {
        // ADR-022 §Core owns: plugins bind to core semantics by listing a reserved
        // edge kind; the parser must NOT reject it.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = ["contains", "calls"]
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let manifest = parse_manifest(toml.as_bytes()).unwrap();
        assert!(
            manifest
                .ontology
                .edge_kinds
                .contains(&"contains".to_owned())
        );
        assert!(manifest.ontology.edge_kinds.contains(&"calls".to_owned()));
    }

    // ── Positive: [integrations.*] passthrough ────────────────────────────────

    #[test]
    fn positive_integrations_block_passthrough_does_not_error() {
        // WP3's plugin.toml adds [integrations.wardline]; must parse without error.
        let toml = r#"
[plugin]
name = "clarion-plugin-python"
plugin_id = "python"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-python"
language = "python"
extensions = ["py"]

[capabilities.runtime]
expected_max_rss_mb = 512
expected_entities_per_file = 5000
wardline_aware = true
reads_outside_project_root = false

[ontology]
entity_kinds = ["function"]
edge_kinds = []
rule_id_prefix = "CLA-PY-"
ontology_version = "0.1.0"

[integrations.wardline]
min_version = "0.1.0"
max_version = "1.0.0"
"#;
        let manifest = parse_manifest(toml.as_bytes()).unwrap();
        // The integrations table is present; the core does not interpret it.
        assert!(manifest.integrations.contains_key("wardline"));
    }

    // ── Negative: too many [integrations.*] entries ───────────────────────────

    /// A `plugin.toml` with more than [`MAX_INTEGRATIONS`] entries is rejected
    /// as `Malformed` — prevents an attacker-controlled manifest from forcing
    /// the host to retain an unbounded `BTreeMap<String, toml::Value>` for
    /// the run lifetime.
    #[test]
    fn negative_integrations_above_cap_is_rejected() {
        use std::fmt::Write;
        let mut toml = String::from(
            r#"[plugin]
name = "clarion-plugin-x"
plugin_id = "x"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = ["x"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-X-"
ontology_version = "0.1.0"

"#,
        );
        for i in 0..=MAX_INTEGRATIONS {
            write!(toml, "[integrations.entry_{i}]\nk = \"v\"\n\n").unwrap();
        }
        let err = parse_manifest(toml.as_bytes())
            .expect_err("manifest with > MAX_INTEGRATIONS integrations must be rejected");
        assert!(
            matches!(err, ManifestError::Malformed { ref message }
                if message.contains("integrations") && message.contains("maximum")),
            "expected Malformed integrations-cap error; got {err:?}"
        );
    }

    // ── Positive: plugin_id can differ from name ──────────────────────────────

    #[test]
    fn positive_plugin_id_can_differ_from_name() {
        // Verifies that [plugin].name (hyphens OK) and plugin_id (kind grammar)
        // are independently valid. This is the exact case that caused the
        // wp2/wp3 contradiction: name = "clarion-plugin-python" (hyphens) while
        // the entity_id needed the segment "python".
        let toml = r#"
[plugin]
name = "clarion-plugin-python"
plugin_id = "python"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-python"
language = "python"
extensions = ["py"]

[capabilities.runtime]
expected_max_rss_mb = 512
expected_entities_per_file = 5000
wardline_aware = true
reads_outside_project_root = false

[ontology]
entity_kinds = ["function"]
edge_kinds = []
rule_id_prefix = "CLA-PY-"
ontology_version = "0.1.0"
"#;
        let manifest = parse_manifest(toml.as_bytes()).unwrap();
        assert_eq!(manifest.plugin.name, "clarion-plugin-python");
        assert_eq!(manifest.plugin.plugin_id, "python");
    }

    // ── Negative: missing plugin_id ───────────────────────────────────────────

    #[test]
    fn negative_missing_plugin_id_returns_malformed() {
        // A manifest without [plugin].plugin_id must fail deserialization because
        // plugin_id is a required field (no serde default).
        let toml = r#"
[plugin]
name = "my-plugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(err, ManifestError::Malformed { .. }));
    }

    // ── Negative: plugin_id with hyphen rejected ──────────────────────────────

    #[test]
    fn negative_plugin_id_with_hyphen_rejected_as_malformed() {
        // "my-plugin" contains a hyphen; the ADR-022 kind grammar [a-z][a-z0-9_]*
        // forbids it. This is the exact contradiction that motivated separating
        // plugin_id from name.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "my-plugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation {
                field: "plugin_id",
                ref value,
            } if value == "my-plugin"
        ));
    }

    // ── Negative: missing [plugin].name ──────────────────────────────────────

    #[test]
    fn negative_missing_plugin_name_returns_malformed() {
        let toml = r#"
[plugin]
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(err, ManifestError::Malformed { .. }));
    }

    // ── Negative: expected_max_rss_mb = 0 ────────────────────────────────────

    #[test]
    fn negative_zero_rss_mb_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 0
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(
            matches!(err, ManifestError::Malformed { message } if message.contains("expected_max_rss_mb"))
        );
    }

    // ── Negative: entity_kinds = [] ──────────────────────────────────────────

    #[test]
    fn negative_empty_entity_kinds_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = []
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(
            matches!(err, ManifestError::Malformed { message } if message.contains("entity_kinds"))
        );
    }

    // ── Negative: malformed entity kind — uppercase ───────────────────────────

    #[test]
    fn negative_entity_kind_uppercase_is_grammar_violation() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["Function"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "entity_kinds", value } if value == "Function"
        ));
    }

    #[test]
    fn negative_entity_kind_hyphen_is_grammar_violation() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["func-tion"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "entity_kinds", value } if value == "func-tion"
        ));
    }

    #[test]
    fn negative_entity_kind_digit_prefix_is_grammar_violation() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["1function"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "entity_kinds", value } if value == "1function"
        ));
    }

    // ── Negative: malformed rule_id_prefix ───────────────────────────────────

    #[test]
    fn negative_rule_id_prefix_no_cla_prefix_rejected() {
        // "PY-" — does not start with CLA.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "PY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "rule_id_prefix", value } if value == "PY-"
        ));
    }

    #[test]
    fn negative_rule_id_prefix_lowercase_rejected() {
        // "cla-py-" — lowercase is invalid.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "cla-py-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "rule_id_prefix", value } if value == "cla-py-"
        ));
    }

    #[test]
    fn negative_rule_id_prefix_mixed_case_segment_rejected() {
        // "CLA-py-" — mixed-case segment after CLA.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-py-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "rule_id_prefix", value } if value == "CLA-py-"
        ));
    }

    // ── Negative: reserved entity kinds ──────────────────────────────────────

    #[test]
    fn negative_reserved_entity_kind_file_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["file", "widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-RESERVED-KIND");
        assert!(matches!(
            err,
            ManifestError::ReservedKind { kind } if kind == "file"
        ));
    }

    #[test]
    fn negative_reserved_entity_kind_subsystem_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["subsystem"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-RESERVED-KIND");
        assert!(matches!(
            err,
            ManifestError::ReservedKind { kind } if kind == "subsystem"
        ));
    }

    #[test]
    fn negative_reserved_entity_kind_guidance_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["guidance"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-RESERVED-KIND");
        assert!(matches!(
            err,
            ManifestError::ReservedKind { kind } if kind == "guidance"
        ));
    }

    // ── Negative: reserved rule_id_prefix ────────────────────────────────────

    #[test]
    fn negative_reserved_prefix_cla_infra_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-INFRA-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-RULE-ID-NAMESPACE");
        assert!(matches!(
            err,
            ManifestError::ReservedPrefix { prefix } if prefix == "CLA-INFRA-"
        ));
    }

    #[test]
    fn negative_reserved_prefix_cla_fact_rejected() {
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-FACT-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-RULE-ID-NAMESPACE");
        assert!(matches!(
            err,
            ManifestError::ReservedPrefix { prefix } if prefix == "CLA-FACT-"
        ));
    }

    // ── Negative: reads_outside_project_root = true (via validate_for_v0_1) ──

    #[test]
    fn negative_reads_outside_project_root_flagged_by_validator() {
        // The parser accepts this field faithfully; the validator rejects it.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = true

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#;
        // parse_manifest must succeed — the parser does not reject this field.
        let manifest = parse_manifest(toml.as_bytes()).unwrap();
        assert!(manifest.capabilities.runtime.reads_outside_project_root);

        // validate_for_v0_1 must surface the unsupported-capability error.
        let err = manifest.validate_for_v0_1().unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY");
        assert!(matches!(
            err,
            ManifestError::UnsupportedCapability {
                capability: "reads_outside_project_root"
            }
        ));
    }

    // ── subcode coverage ──────────────────────────────────────────────────────

    #[test]
    fn subcode_returns_correct_string_for_each_variant() {
        assert_eq!(
            ManifestError::Malformed {
                message: String::new()
            }
            .subcode(),
            "CLA-INFRA-MANIFEST-MALFORMED"
        );
        assert_eq!(
            ManifestError::GrammarViolation {
                field: "entity_kinds",
                value: String::new()
            }
            .subcode(),
            "CLA-INFRA-MANIFEST-MALFORMED"
        );
        assert_eq!(
            ManifestError::ReservedKind {
                kind: String::new()
            }
            .subcode(),
            "CLA-INFRA-MANIFEST-RESERVED-KIND"
        );
        assert_eq!(
            ManifestError::ReservedPrefix {
                prefix: String::new()
            }
            .subcode(),
            "CLA-INFRA-RULE-ID-NAMESPACE"
        );
        assert_eq!(
            ManifestError::UnsupportedCapability { capability: "x" }.subcode(),
            "CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY"
        );
    }

    // ── Grammar edge cases ────────────────────────────────────────────────────

    #[test]
    fn negative_rule_id_prefix_no_trailing_hyphen_rejected() {
        // "CLA-PY" — missing trailing `-`.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-PY"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation {
                field: "rule_id_prefix",
                ..
            }
        ));
    }

    #[test]
    fn negative_rule_id_prefix_empty_inner_segment_rejected() {
        // "CLA--PY-" — empty segment between hyphens.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA--PY-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation {
                field: "rule_id_prefix",
                ..
            }
        ));
    }

    #[test]
    fn negative_rule_id_prefix_only_cla_rejected() {
        // "CLA-" — no segment after CLA.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(toml.as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation {
                field: "rule_id_prefix",
                ..
            }
        ));
    }

    #[test]
    fn positive_multi_segment_rule_id_prefix_valid() {
        // "CLA-FOO-BAR-" — valid multi-segment prefix.
        let toml = r#"
[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = ["my"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-FOO-BAR-"
ontology_version = "0.1.0"
"#;
        let manifest = parse_manifest(toml.as_bytes()).unwrap();
        assert_eq!(manifest.ontology.rule_id_prefix, "CLA-FOO-BAR-");
    }

    // ── Extension format grammar (ticket clarion-fa35cad487) ─────────────────

    fn ext_manifest(extensions_toml: &str) -> String {
        format!(
            r#"[plugin]
name = "my-plugin"
plugin_id = "myplugin"
version = "0.1.0"
protocol_version = "1.0"
executable = "my-plugin"
language = "mylang"
extensions = {extensions_toml}

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "CLA-MY-"
ontology_version = "0.1.0"
"#
        )
    }

    #[test]
    fn positive_extension_lowercase_alphanumeric_accepted() {
        let manifest = parse_manifest(ext_manifest(r#"["py"]"#).as_bytes()).unwrap();
        assert_eq!(manifest.plugin.extensions, vec!["py"]);
    }

    #[test]
    fn negative_extension_uppercase_rejected() {
        let err = parse_manifest(ext_manifest(r#"["PY"]"#).as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "extensions", value } if value == "PY"
        ));
    }

    #[test]
    fn negative_extension_with_dot_rejected() {
        let err = parse_manifest(ext_manifest(r#"[".py"]"#).as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "extensions", value } if value == ".py"
        ));
    }

    #[test]
    fn negative_extension_empty_string_rejected() {
        let err = parse_manifest(ext_manifest(r#"[""]"#).as_bytes()).unwrap_err();
        assert_eq!(err.subcode(), "CLA-INFRA-MANIFEST-MALFORMED");
        assert!(matches!(
            err,
            ManifestError::GrammarViolation { field: "extensions", value } if value.is_empty()
        ));
    }
}
