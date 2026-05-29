# Full-codebase review sweep — 2026-05-29

A multi-agent code-review sweep over Clarion's own source + tests, chunked into
~2000-line semantically-coherent units. Each chunk is reviewed by a team of
five agents (architecture critic, systems thinker, language engineer, quality
engineer, + a chunk-specific specialist). Findings are deduped per chunk and
filed as Filigree issues.

**Scope:** Clarion's own code only. The `tests/perf/elspeth_mini/` customer
fixture corpus (~33k lines) is **excluded** — it is representative customer
code, not Clarion source.

**Filigree tagging:** every issue gets `from-review-sweep-2026-05-29` (batch
source, for bulk triage/revert), a `chunk:<id>` label, a `review:<severity>`
label (`blocker|high|medium|low|suggestion`), plus `crate:*` / `category:*` /
`plugin:python` as applicable. Defects → type `bug`; improvements/suggestions →
type `task`. Priority: blocker=0, high=1, medium=2, low=3, suggestion=4.

**Team roster per chunk:** `architecture-critic`, `plan-review-systems`
(systems thinker), language engineer (`code-reviewer` for Rust /
`python-code-reviewer` for Python), quality engineer
(`quality-assurance-analyst` for src / `test-suite-reviewer` for tests), and
one chunk-specific specialist (see table). A sixth agent per chunk synthesizes
+ dedupes the five reports and files the tickets.

## Chunk manifest (35 chunks)

### Rust — clarion-core src
| id | files / ranges | specialist |
|----|----------------|------------|
| core-host-a | `clarion-core/src/plugin/host.rs:1-1470` | threat-analyst |
| core-host-b | `clarion-core/src/plugin/host.rs:1471-2935` | silent-failure-hunter |
| core-llm | `clarion-core/src/llm_provider.rs` (2467) | silent-failure-hunter |
| core-manifest-protocol | `manifest.rs` (1119) + `plugin/protocol.rs` (875) | type-design-analyzer |
| core-mock-discovery-transport | `plugin/mock.rs` (876) + `discovery.rs` (667) + `transport.rs` (569) | code-reviewer |
| core-entityid-jail-limits | `entity_id.rs` (596) + `plugin/limits.rs` (572) + `breaker.rs` (360) + `jail.rs` (260) + `host_findings.rs` (273) + `mod.rs` + `lib.rs` | threat-analyst |

### Rust — clarion-cli src
| id | files / ranges | specialist |
|----|----------------|------------|
| cli-analyze-a | `clarion-cli/src/analyze.rs:1-1220` | code-reviewer |
| cli-analyze-b | `clarion-cli/src/analyze.rs:1221-2433` | code-reviewer |
| cli-http-read | `clarion-cli/src/http_read.rs` (1736) | api-reviewer |
| cli-secret-scan | `secret_scan.rs` + `secret_scan/{findings,files,anchors,baseline}.rs` + `serve.rs` + `instance.rs` | threat-analyst |
| cli-misc | `clustering.rs` + `install.rs` + `skill_pack.rs` + `analyze_lock.rs` + `config.rs` + `main.rs` + `cli.rs` + `stats.rs` + `run_lifecycle.rs` | code-reviewer |

### Rust — clarion-mcp src
| id | files / ranges | specialist |
|----|----------------|------------|
| mcp-lib-a | `clarion-mcp/src/lib.rs:1-1150` | api-reviewer |
| mcp-lib-b | `clarion-mcp/src/lib.rs:1151-2300` | api-reviewer |
| mcp-lib-c | `clarion-mcp/src/lib.rs:2301-3449` | api-reviewer |
| mcp-config-filigree | `config.rs` (956) + `filigree.rs` (238) | code-reviewer |

### Rust — clarion-storage src
| id | files / ranges | specialist |
|----|----------------|------------|
| storage-query | `query.rs` (1097) + `schema.rs` (209) + `pragma.rs` (109) | embedded-database-reviewer |
| storage-writer | `writer.rs` (1080) + `cache.rs` + `commands.rs` + `error.rs` + `reader.rs` + `unresolved.rs` + `lib.rs` | embedded-database-reviewer |

### Rust — clarion-scanner + fixture src
| id | files / ranges | specialist |
|----|----------------|------------|
| scanner-src | `clarion-scanner/src/*` (patterns/baseline/lib/entropy) + `clarion-plugin-fixture/src/*` | threat-analyst |

### Python plugin src
| id | files / ranges | specialist |
|----|----------------|------------|
| py-pyright | `pyright_session.py` (1427) + `qualname.py` | python-code-reviewer |
| py-extractor | `extractor.py` (918) + `server.py` + `entity_id.py` + `reference_resolver.py` + `call_resolver.py` + `wardline_probe.py` + `stdout_guard.py` + `__main__.py` | python-code-reviewer |

### Rust tests
| id | files / ranges | specialist |
|----|----------------|------------|
| test-serve-a | `clarion-cli/tests/serve.rs:1-1340` | api-reviewer |
| test-serve-b | `clarion-cli/tests/serve.rs:1341-2683` | api-reviewer |
| test-writer-actor-a | `clarion-storage/tests/writer_actor.rs:1-1235` | embedded-database-reviewer |
| test-writer-actor-b | `clarion-storage/tests/writer_actor.rs:1236-2471` | embedded-database-reviewer |
| test-storage-tools-a | `clarion-mcp/tests/storage_tools.rs:1-1115` | api-reviewer |
| test-storage-tools-b | `clarion-mcp/tests/storage_tools.rs:1116-2233` | api-reviewer |
| test-cli-analyze | `analyze.rs` (1221) + `analyze_failure_modes.rs` + `install.rs` | coverage-gap-analyst |
| test-storage-query | `query_helpers.rs` (1082) + `reader_pool.rs` + `llm_cache.rs` | embedded-database-reviewer |
| test-schema-host | `schema_apply.rs` (1051) + `core/tests/host_subprocess.rs` | embedded-database-reviewer |
| test-secret-scanner | `cli/tests/secret_scan.rs` (917) + `scanner/tests/scanner.rs` | threat-analyst |

### Python tests
| id | files / ranges | specialist |
|----|----------------|------------|
| pytest-extractor | `test_extractor.py` (1203) + `test_qualname.py` | coverage-gap-analyst |
| pytest-pyright | `test_pyright_session.py` (968) + `test_stdout_guard.py` + `test_package.py` | coverage-gap-analyst |
| pytest-server | `test_server.py` (586) + `test_round_trip.py` + `test_entity_id.py` + `test_wardline_probe.py` | coverage-gap-analyst |

### Scripts + e2e
| id | files / ranges | specialist |
|----|----------------|------------|
| scripts-governance | `scripts/*.py` + `scripts/*.sh` (governance/CI) | pipeline-reviewer |
| e2e-shell | `tests/e2e/*.sh` + `cli/tests/{wp2_e2e,wp1_e2e,skills}.rs` | pipeline-reviewer |
