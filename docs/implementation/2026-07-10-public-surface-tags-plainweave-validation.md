# Public Surface Tags for Plainweave Validation - 2026-07-10

## Summary

Plainweave was already honestly reporting denominator incompleteness. This
validation checks that Loomweave now sees more public doors and that any lower
Plainweave ratio is treated as a more accurate denominator, not a Plainweave
defect.

## Artifact Preflight

Commands run from `/home/john/loomweave`:

```text
cargo build --workspace --bins
uv sync --project plugins/python --locked --extra dev
plugins/python/.venv/bin/python -m pip install --quiet --force-reinstall -e 'plugins/python[dev]'
```

Results:

```text
loomweave = /home/john/loomweave/target/debug/loomweave
loomweave-plugin-python = /home/john/loomweave/plugins/python/.venv/bin/loomweave-plugin-python
plugins/python/.venv/share/loomweave/plugins/python/plugin.toml:4:version = "1.4.1"
plugins/python/.venv/share/loomweave/plugins/python/plugin.toml:61:ontology_version = "0.11.0"
```

The validation commands below used:

```text
PATH=/home/john/loomweave/plugins/python/.venv/bin:/home/john/loomweave/target/debug:$PATH
```

## /home/john/scrappack

`loomweave analyze .` used the intended plugin:

```text
discovered plugin plugin_id=python executable=/home/john/loomweave/plugins/python/.venv/bin/loomweave-plugin-python
plugin tag-schema marker changed since last run; forcing full re-dispatch of this plugin's files ... ontology_version=0.11.0
analyze complete: run a7eb04c7-6bff-494f-8c35-b9403b7647d6 completed (graph: 195 entities incl. 4 subsystems, 488 edges)
```

Tracked working tree status:

```text
before:  M .plainweave/plainweave.db
after:   M .plainweave/plainweave.db
```

That tracked Plainweave DB change was already present before validation. The
Loomweave catalog mutation is evidenced separately because `.weft/` is ignored.

Catalog file evidence:

| Moment | Size | Mtime | SHA-256 |
|---|---:|---:|---|
| Before | 1257472 | 1783647317 | `e787ea64162aae1356d28e665a35cb0580951673069e07ba40394604fac7d1b1` |
| After | 1372160 | 1783663878 | `f7b383a42babb114efb82e6926d7c2b12a1c441ac659623fa4371e9f0e60a6db` |

Tag counts:

| Tag | Before | After |
|---|---:|---:|
| `cli-command` | 0 | 5 |
| `entry-point` | 4 | 5 |
| `exported-api` | 0 | 0 |
| `framework-handler` | 0 | 0 |
| `http-route` | 0 | 0 |
| `public-surface` | 100 | 100 |

Public-surface examples now present:

```text
python:function:probe_a.cli.run|cli-command,entry-point
python:function:probe_a.smoke_test.main|cli-command,entry-point
python:function:probe_b.smoke_test.main|cli-command,entry-point
python:function:rustfang.playtest_cli.main|cli-command,entry-point
python:function:rustfang.smoke_test.main|cli-command,entry-point
```

`http-route` and `exported-api` remain absent in this corpus. This is source
reality for `/home/john/scrappack`, not a Loomweave or Plainweave defect; this
repo is evidence for the newly visible manual CLI surfaces.

Plainweave output:

```text
ok=true
north_star numerator=4 denominator=5 ratio=0.8
coverage.public_surface_tags=["cli-command","entry-point","exported-api","http-route"]
coverage.present_tags=["cli-command","entry-point"]
coverage.absent_tags=["exported-api","http-route"]
coverage.complete=false
adapter.degraded[0].code="public_surface_tags_incomplete"
adapter.degraded[0].message="Some public-surface tag classes are absent from the local Loomweave catalog, so this enumeration may under-report exported entities. Absent tag classes: exported-api, http-route"
```

## /home/john/scrappack-engine-phase-tasks-1-2

`loomweave analyze .` used the intended plugin:

```text
discovered plugin plugin_id=python executable=/home/john/loomweave/plugins/python/.venv/bin/loomweave-plugin-python
plugin tag-schema marker changed since last run; forcing full re-dispatch of this plugin's files ... ontology_version=0.11.0
analyze complete: run 3c76f9cd-0d5e-491e-a588-17b08b8058d1 completed (graph: 805 entities incl. 93 subsystems, 2424 edges)
```

Tracked working tree status:

```text
before: clean
after:  clean
```

The Loomweave catalog mutation is evidenced separately because `.weft/` is
ignored.

Catalog file evidence:

| Moment | Size | Mtime | SHA-256 |
|---|---:|---:|---|
| Before | 5234688 | 1783573094 | `9697e379cd7a13413ff6f13f4906c74ca08359a912c03169ee1f56b0482bf749` |
| After | 5296128 | 1783663949 | `10f0749fee8188c9473e2643725bbe7ba5f73b33eadc6f55f36b899f171c5e9c` |

Tag counts:

| Tag | Before | After |
|---|---:|---:|
| `cli-command` | 0 | 7 |
| `entry-point` | 6 | 7 |
| `exported-api` | 0 | 1 |
| `framework-handler` | 13 | 13 |
| `http-route` | 13 | 13 |
| `public-surface` | 253 | 253 |

Public-surface examples now present:

```text
python:function:probe_a.cli.run|cli-command,entry-point
python:function:probe_a.smoke_test.main|cli-command,entry-point
python:function:probe_b.smoke_test.main|cli-command,entry-point
python:function:rustfang.playtest_cli.main|cli-command,entry-point
python:function:rustfang.smoke_test.main|cli-command,entry-point
python:function:rustfang.tuning.run_gate.main|cli-command,entry-point
python:function:rustfang.web.harness.main|cli-command,entry-point
python:function:rustfang.web.routes.campaign.campaign_page|http-route
python:function:rustfang.web.routes.council.council_page|http-route
python:function:rustfang.web.routes.court.court_page|http-route
python:function:rustfang.web.routes.lobby.create_campaign_page|http-route
python:function:rustfang.web.routes.lobby.join_campaign_page|http-route
python:function:rustfang.web.routes.lobby.lobby_page|http-route
python:function:rustfang.web.routes.map.map_page|http-route
python:function:rustfang.web.routes.orders.order_page|http-route
python:function:rustfang.web.routes.orders.submit_order_page|http-route
python:function:rustfang.web.routes.report.report_page|http-route
python:function:rustfang.web.routes.standings.standings_page|http-route
python:function:rustfang.web.routes.workshop.workshop_page|http-route
python:function:rustfang.web.sse.campaign_events|http-route
python:module:rustfang.engine.oracle|exported-api
```

Plainweave output:

```text
ok=false
error.code="NOT_FOUND"
error.message="Plainweave project is not initialized"
error.hint="Run `plainweave init` in this project and retry."
error.details.db_path="/home/john/scrappack-engine-phase-tasks-1-2/.plainweave/plainweave.db"
```

That is out of scope for this validation unless the owner initializes
Plainweave in the engine-phase repo. The Loomweave SQLite catalog still proves
the source denominator rows that Plainweave would consume.

## Attribution

Plainweave good. Requirements good. Loomweave must see all doors.
