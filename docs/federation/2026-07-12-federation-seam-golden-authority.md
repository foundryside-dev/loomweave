# Federation seam golden authority

Loomweave is the producer authority for these normative fixtures. Plainweave
ticket `plainweave-f8303b4b50` owns copying the exact bytes into Plainweave and
landing its repository-owned byte oracle; that downstream ticket is not
satisfied merely because Loomweave's read-only consumer harness passes.

| Fixture | SHA-256 |
| --- | --- |
| `crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json` | `f818252e8a6fd28d8890014cb8f8eccc76de86f8448202ed8fe5a56f364c8d6f` |
| `docs/federation/fixtures/classification.python.json` | `2b3822d1bd3db4f986646f04d269cad52ccfa9c85257bedb567aab1ef2c4d3c5` |
| `docs/federation/fixtures/get-api-v1-capabilities.json` | `61020b20aadaef75a3de523f0a8f83be03d1d503ffdca719c78d949d20beeced` |
| `docs/federation/fixtures/loomweave-http-auth-v1.golden.json` | `cd4a8a1598bedafdfe247d47a616e9a82a148e7cf8feaac9299a21550b2c720b` |
| `docs/federation/fixtures/external-sqlite-compatibility-v1.json` | `a69eea6d887328faf973dbde375fc56b73c45608191dc0db37511cc0aadfe10e` |
| `docs/federation/fixtures/identity-ownership-v1.golden.json` | `919d5a73723b42406788e14675aa8fe48dfb9a3b6412ea3b2ef35a8065d7656b` |

Regenerate from this checkout with:

```bash
scripts/generate-federation-seam-goldens.sh
```

The generator builds the workspace binaries, force-refreshes the Python
plugin's installed Hatch shared data, and invokes the explicit worktree
artifacts recorded in `classification.python.json`. It analyzes
`tests/fixtures/federation_classifier_python/` through the real Python plugin
and calls the real MCP `entity_tag_list` handler for all four public-surface
classifiers. The required result is `5/5/0/0`; both zero-count classifiers must
still be `supported` and `complete`.

Only volatile producer facts are normalized: the analysis run UUID becomes
`<run-id>`, the temporary analysis root becomes `<fixture-root>`, run timestamps
are omitted, the checkout root in artifact metadata becomes `<repo-root>`, and
freshly minted SEIs (whose mint input contains that run UUID) become unmistakable
`normalized-sei:<locator-key>` opaque placeholders. These placeholders are not
Loomweave SEIs and do not claim to reproduce the production BLAKE3 mint; the
generator first validates that each input has the real `loomweave:eid:` plus
32-lowercase-hex production shape before replacing it. No counts, coverage flags,
plugin declarations, entity IDs, content hashes, pagination flags, ownership
fields, auth bytes, or compatibility reports are normalized.

Every JSON file is pretty-printed with a trailing newline and has a neighbouring
`.sha256` sidecar. The owning Rust tests recompute each digest, prove a one-byte
mutation is rejected, and recheck the compatibility/auth/HTTP shapes against
production serializers or handlers.

Inherited operator credentials cannot change the output. The unauthenticated
handler run names a dedicated `LOOMWEAVE_GOLDEN_NO_AUTH_UNSET` bearer-token env
and the generator removes both that name and `WEFT_TOKEN` from the child. The
poison regression is:

```bash
scripts/check-federation-seam-goldens-hermetic.sh
```

Run the downstream adapter without modifying Plainweave:

```bash
before=$(git -C /home/john/plainweave status --porcelain=v1)
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=/home/john/plainweave/src \
  /home/john/plainweave/.venv/bin/python \
  scripts/validate-plainweave-classifier-golden.py \
  --plainweave-root /home/john/plainweave
test "$(git -C /home/john/plainweave status --porcelain=v1)" = "$before"
```

The harness creates its compatible SQLite catalogue only under an external
temporary directory, embeds the original coverage golden byte sequence directly
inside the `runs.stats` object without parsing or compact reserialization,
hashes the database plus SQLite sidecars before and after the adapter call, and
invokes Plainweave's real `LoomweaveAdapter.list_catalog` path. For any
Plainweave pytest follow-up, also set `PYTHONDONTWRITEBYTECODE=1`, disable the
pytest cache plugin, and put `--basetemp` under `/tmp`.
