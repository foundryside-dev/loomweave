//! Shared ontology vocabulary emitted by plugins and consumed by host tools.

pub mod tags {
    //! Categorisation tag strings shared across plugin emitters and host readers.

    pub const ENTRY_POINT: &str = "entry-point";
    pub const HTTP_ROUTE: &str = "http-route";
    pub const TEST: &str = "test";
    pub const DATA_MODEL: &str = "data-model";
    pub const CLI_COMMAND: &str = "cli-command";
    pub const EXPORTED_API: &str = "exported-api";
    pub const PUBLIC_SURFACE: &str = "public-surface";
    pub const ALLOW_DEAD_CODE: &str = "allow-dead-code";
    pub const WARDLINE_EXTERNAL_BOUNDARY: &str = "wardline:external_boundary";
    pub const WARDLINE_TRUSTED: &str = "wardline:trusted";
    pub const DYNAMIC_DISPATCH: &str = "dynamic-dispatch";
    pub const REFLECTION: &str = "reflection";
    pub const FRAMEWORK_HANDLER: &str = "framework-handler";
    pub const PLUGIN_HOOK: &str = "plugin-hook";

    /// Tags whose union seeds dead-code reachability roots: entities reached from
    /// outside the static call/import graph.
    pub const DEAD_CODE_ROOTS: &[&str] = &[
        ENTRY_POINT,
        HTTP_ROUTE,
        TEST,
        DATA_MODEL,
        CLI_COMMAND,
        EXPORTED_API,
        PUBLIC_SURFACE,
        ALLOW_DEAD_CODE,
        WARDLINE_EXTERNAL_BOUNDARY,
        WARDLINE_TRUSTED,
    ];

    /// Tags that force liveness because static reachability is known incomplete.
    pub const DEAD_CODE_BARRIERS: &[&str] = &[DYNAMIC_DISPATCH, REFLECTION];
    /// Tags excluded from dead-code candidacy because their callers are framework
    /// or plugin magic outside the static graph.
    pub const DEAD_CODE_EXCLUDED: &[&str] = &[FRAMEWORK_HANDLER, PLUGIN_HOOK];
}
