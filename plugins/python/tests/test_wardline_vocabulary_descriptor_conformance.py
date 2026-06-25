"""Wardline → Loomweave trust-vocabulary descriptor wire conformance oracle.

The CONSUMER side of the cross-repo "Vocabulary descriptor (trust-vocab)" seam.
Wardline AUTHORS the NG-25 trust-vocabulary descriptor — it OWNS the vocabulary
via ``wardline.core.registry.REGISTRY`` and serialises it through
``wardline.core.descriptor.build_vocabulary_descriptor`` / ``descriptor_to_yaml``
to ``.weft/wardline/vocabulary.yaml`` (``{schema: wardline.vocabulary/v1,
version, entries:[{canonical_name, group, attrs}]}``). Loomweave's Python plugin
is the real CONSUMER: ``loomweave_plugin_python.wardline_descriptor
.load_wardline_descriptor`` byte-reads that file (it NEVER imports wardline),
version-gates on ``EXPECTED_DESCRIPTOR_VERSION == "wardline-generic-2"``, and
parses ``entries`` into a ``WardlineVocabulary`` that the extractor threads into
``wardline:external_boundary`` / ``wardline:trusted`` entity tags.

This oracle pins that the bytes Wardline produces are accepted + correctly
interpreted by Loomweave's REAL consumer code path. It mirrors the layering of
the taint-fact storage oracle
(``crates/loomweave-storage/tests/wardline_taint_fact_conformance_oracle.rs``):

  * Layer 1 — a byte-pin (``test_vendored_golden_matches_byte_pin``): the
    git-blob SHA-1 of the vendored golden, asserted against a const. If the
    vendored fixture drifts by a single byte the pin reds. The SHA mirrors
    wardline's own ``UPSTREAM_BLOB_SHA`` byte-pin idiom
    (``wardline/tests/conformance/test_vocabulary_descriptor_wire_golden.py``)
    so the two repos pin the SAME 40 hex chars. On its OWN this is circular
    (it pins the vendored bytes against themselves); the non-circular breaks
    are the consumer oracle below + the Layer-2 drift recheck.

  * A NON-CIRCULAR consumer oracle: the vendored golden bytes are written to a
    project descriptor location and fed through Loomweave's REAL
    ``load_wardline_descriptor`` (resolve → read → ``yaml.safe_load`` → parse →
    version-gate), asserting Loomweave ACCEPTS them (version accepted →
    ``enabled``; entries → ``WardlineVocabulary`` with the expected
    canonical_names / groups; the ``external_boundary`` / ``trusted`` decorator
    attrs derive correctly). It then drives the REAL extractor
    (``extractor.extract``) with that parsed-from-golden vocabulary to prove the
    ``wardline:external_boundary`` / ``wardline:trusted`` TAGS derive correctly
    end to end — not just that the lookup table parses. A version-skew copy
    (only the ``version`` string bumped, every other byte the golden's) is fed
    through the SAME real loader to prove the version gate is real, not
    cosmetic: ``enabled`` flips to ``version_skew``. All assertions are driven
    off the consumer's returned state, NOT off the golden restated against
    itself.

  * Layer 2 — a drift recheck (``test_vendored_golden_matches_wardline_authority``):
    the vendored fixture bytes are compared against the authority golden in the
    Wardline repo
    (``$WARDLINE_REPO/tests/conformance/fixtures/wardline-vocabulary-descriptor.golden.yaml``,
    ``WARDLINE_REPO`` defaulting to ``/home/john/wardline``). Skip-clean when the
    sibling repo is absent (CI / detached checkout); fail-closed on any
    divergence.

── Scope / honesty caveat ──
This drives the consumer parse + gate (``load_wardline_descriptor``) and the
consumer tag-derivation (``extractor.extract`` with the parsed vocabulary) — the
same two code paths the plugin server runs: ``server.handle_initialize`` calls
``load_wardline_descriptor`` and stashes ``.vocabulary`` on its state, and
``server.handle_analyze_file`` threads that vocabulary into ``extract`` as
``wardline_vocabulary=``. It does NOT spin up the JSON-RPC server loop itself;
the server's own tests cover that wiring. The non-circular guarantee comes from
driving the REAL parse/gate/derive on the producer-authored bytes, never the
golden against itself.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

import pytest

from loomweave_plugin_python.extractor import extract
from loomweave_plugin_python.wardline_descriptor import (
    EXPECTED_DESCRIPTOR_VERSION,
    WardlineVocabulary,
    load_wardline_descriptor,
)

# The vendored copy of wardline's authority golden, BYTE-IDENTICAL to
# wardline/tests/conformance/fixtures/wardline-vocabulary-descriptor.golden.yaml
# (confirmed via `cmp`). Read as bytes so the byte-pin sees the exact on-disk
# bytes wardline ships, not a yaml.safe_load round-trip.
GOLDEN_PATH = Path(__file__).parent / "fixtures" / "wardline-vocabulary-descriptor.golden.yaml"

# Layer-1 byte-pin: the git-blob SHA-1 of the vendored golden. This is the SAME
# 40 hex chars wardline pins as UPSTREAM_BLOB_SHA on the producer side
# (wardline/tests/conformance/test_vocabulary_descriptor_wire_golden.py) — the
# two repos pin identical bytes, so a one-sided re-vendor reds both suites.
# Recomputed below as sha1(b"blob %d\0" % len(data) + data). Any edit to the
# vendored golden without a matching re-pin reds this test.
UPSTREAM_BLOB_SHA = "f5ad8d2346ffb6ea75aa469e423c6c7cfd16d40a"

# The canonical decorator names the producer authors (group 1) and the attrs the
# consumer must surface for the trust-tier markers. Asserting on these proves the
# consumer parses the real authored format — including the trust-tier `attrs`
# that the existing inline-string unit tests cover but which here come straight
# off the producer's own bytes.
EXPECTED_CANONICAL_NAMES = ("external_boundary", "trust_boundary", "trusted")
EXPECTED_ATTRS = {
    "external_boundary": {},
    "trust_boundary": {"_wardline_to_level": "TaintState"},
    "trusted": {"_wardline_level": "TaintState"},
}


def _write_project_descriptor(project_root: Path, text: str) -> None:
    """Place descriptor bytes at the real .weft/wardline/ project location the
    consumer reads (ADR-046)."""
    descriptor = project_root / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True, exist_ok=True)
    descriptor.write_text(text, encoding="utf-8")


# ── Layer 1 — byte-pin ───────────────────────────────────────────────────────


def test_vendored_golden_matches_byte_pin() -> None:
    """Layer-1: the vendored wardline-authored descriptor golden byte-pins to its
    git-blob SHA-1. ANY edit to the vendored fixture without a matching re-pin
    reds here. On its OWN this is circular (vendored bytes pinned against
    themselves); the non-circular protection is the consumer oracle + the Layer-2
    drift recheck below.

    Tamper proof (verified out-of-band): a one-byte-tampered copy of the fixture
    hashes to a DIFFERENT git-blob SHA-1, so this assert reds — the pin is
    load-bearing, not decorative.
    """
    assert len(UPSTREAM_BLOB_SHA) == 40, (
        f"UPSTREAM_BLOB_SHA must be a 40-char git blob SHA-1: {UPSTREAM_BLOB_SHA!r}"
    )
    assert set(UPSTREAM_BLOB_SHA) <= set("0123456789abcdef"), (
        f"UPSTREAM_BLOB_SHA must be lowercase hex (a git blob SHA-1): {UPSTREAM_BLOB_SHA!r}"
    )
    data = GOLDEN_PATH.read_bytes()
    actual = hashlib.sha1(b"blob %d\x00" % len(data) + data).hexdigest()  # noqa: S324 - git blob id, not a security hash
    assert actual == UPSTREAM_BLOB_SHA, (
        f"the vendored vocabulary-descriptor golden changed (git blob {actual}, "
        f"pinned {UPSTREAM_BLOB_SHA}) — if this was a deliberate re-vendor, re-copy "
        "BYTE-IDENTICAL from wardline "
        "(tests/conformance/fixtures/wardline-vocabulary-descriptor.golden.yaml), "
        "confirm with `cmp`, and update UPSTREAM_BLOB_SHA in the SAME commit; if not, "
        "revert the edit."
    )


# ── NON-CIRCULAR consumer oracle ─────────────────────────────────────────────


def test_consumer_accepts_golden_and_parses_vocabulary(tmp_path: Path) -> None:
    """The vendored golden bytes (schema line included) fed through the REAL
    ``load_wardline_descriptor`` are ACCEPTED: version-gated to ``enabled`` and
    parsed into a ``WardlineVocabulary`` whose canonical_names / groups / attrs
    match the producer-authored format. Driven off the consumer's returned state,
    never the golden against itself."""
    golden_text = GOLDEN_PATH.read_text("utf-8")
    _write_project_descriptor(tmp_path, golden_text)

    state = load_wardline_descriptor(tmp_path)

    # Version accepted — the gate passed, so the descriptor is enabled.
    assert state.status == "enabled", (
        f"consumer rejected the producer-authored golden: status={state.status!r} "
        f"reason={state.reason!r}"
    )
    assert state.source == "project"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION

    vocab = state.vocabulary
    assert isinstance(vocab, WardlineVocabulary)
    assert vocab.version == EXPECTED_DESCRIPTOR_VERSION
    assert vocab.confidence_basis == "descriptor"

    # entries → WardlineVocabulary with the expected canonical_names.
    assert tuple(sorted(vocab.entries_by_name)) == tuple(sorted(EXPECTED_CANONICAL_NAMES))

    # Each entry's group + attrs survive parse exactly. The trust-tier attrs
    # (_wardline_to_level / _wardline_level) are the real cross-tool delta the
    # inline-string unit tests duplicate but which here come off producer bytes.
    for name in EXPECTED_CANONICAL_NAMES:
        entry = vocab.entries_by_name[name]
        assert entry.canonical_name == name
        assert entry.group == 1, f"{name} must be a group-1 marker, got {entry.group}"
        assert entry.attrs == EXPECTED_ATTRS[name], (
            f"{name} attrs drifted: {entry.attrs!r} != {EXPECTED_ATTRS[name]!r}"
        )

    # entry_for_decorator (the real lookup the extractor calls) resolves the
    # last dotted segment to the right entry — the external_boundary / trusted
    # markers derive from the parsed table, not a restated f-string.
    eb = vocab.entry_for_decorator("weft_markers.external_boundary")
    assert eb is not None
    assert eb.canonical_name == "external_boundary"
    tr = vocab.entry_for_decorator("trusted")
    assert tr is not None
    assert tr.canonical_name == "trusted"


def test_consumer_derives_external_boundary_and_trusted_tags_through_extractor(
    tmp_path: Path,
) -> None:
    """End-to-end consumer derivation: parse the golden through the REAL
    ``load_wardline_descriptor``, then thread the parsed vocabulary into the REAL
    ``extractor.extract``. This proves the ``wardline:external_boundary`` /
    ``wardline:trusted`` TAGS derive correctly through the full consumer path —
    not merely that the lookup table parsed."""
    golden_text = GOLDEN_PATH.read_text("utf-8")
    _write_project_descriptor(tmp_path, golden_text)

    state = load_wardline_descriptor(tmp_path)
    assert state.status == "enabled"
    vocabulary = state.vocabulary
    assert vocabulary is not None

    source = """\
from weft_markers import external_boundary, trust_boundary, trusted

@external_boundary
def read_body():
    return ""

@weft_markers.trust_boundary(to_level="ASSURED")
@trusted(level="INTEGRAL")
class Sanitizer:
    pass
"""

    entities, _edges = extract(source, "service.py", wardline_vocabulary=vocabulary)

    read_body = next(e for e in entities if e["id"] == "python:function:service.read_body")
    sanitizer = next(e for e in entities if e["id"] == "python:class:service.Sanitizer")

    # external_boundary tag derives from the golden-parsed vocabulary.
    assert "wardline" in read_body["tags"]
    assert "wardline:external_boundary" in read_body["tags"]
    assert read_body["wardline"]["descriptor_version"] == EXPECTED_DESCRIPTOR_VERSION
    assert read_body["wardline"]["confidence_basis"] == "descriptor"
    eb_decorators = read_body["wardline"]["decorators"]
    assert [d["canonical_name"] for d in eb_decorators] == ["external_boundary"]
    assert eb_decorators[0]["attrs"] == {}

    # trust_boundary + trusted tags derive, carrying the trust-tier attrs.
    assert "wardline:trust_boundary" in sanitizer["tags"]
    assert "wardline:trusted" in sanitizer["tags"]
    san_attrs = {d["canonical_name"]: d["attrs"] for d in sanitizer["wardline"]["decorators"]}
    assert san_attrs["trust_boundary"] == {"_wardline_to_level": "TaintState"}
    assert san_attrs["trusted"] == {"_wardline_level": "TaintState"}


def test_consumer_version_gate_rejects_skew_copy(tmp_path: Path) -> None:
    """The version gate is REAL, not cosmetic: the SAME golden bytes with ONLY
    the ``version`` string bumped flip the consumer from ``enabled`` to
    ``version_skew`` through the REAL ``load_wardline_descriptor``. The contrast
    with ``test_consumer_accepts_golden_and_parses_vocabulary`` (identical bytes
    bar the version) is the proof that the gate fires on version alone."""
    golden_text = GOLDEN_PATH.read_text("utf-8")
    assert EXPECTED_DESCRIPTOR_VERSION in golden_text, (
        "the golden must carry the expected version for the skew derivation to be a "
        "single-field perturbation"
    )
    skewed = golden_text.replace(EXPECTED_DESCRIPTOR_VERSION, "wardline-generic-3")
    assert skewed != golden_text, "version-skew copy must differ from the golden"
    _write_project_descriptor(tmp_path, skewed)

    state = load_wardline_descriptor(tmp_path)

    # The gate fired: a one-field version bump is REJECTED as skew.
    assert state.status == "version_skew", (
        f"version gate failed to fire on a skewed descriptor: status={state.status!r}"
    )
    assert state.descriptor_version == "wardline-generic-3"
    assert state.expected_version == EXPECTED_DESCRIPTOR_VERSION
    # The vocabulary still parses (entries are valid) but is flagged degraded —
    # proving the gate keys on version, not on a parse failure.
    assert state.vocabulary is not None
    assert state.vocabulary.confidence_basis == "descriptor_version_skew"
    assert tuple(sorted(state.vocabulary.entries_by_name)) == tuple(sorted(EXPECTED_CANONICAL_NAMES))


# ── Layer 2 — drift recheck vs the Wardline source of truth ──────────────────


def test_vendored_golden_matches_wardline_authority() -> None:
    """Layer-2: the vendored fixture bytes must equal the authority golden in the
    Wardline repo. Skip-clean when the sibling repo is absent (CI / detached
    checkout) — the vendored copy + Layer-1 pin still hold; fail-closed on any
    divergence."""
    repo = os.environ.get("WARDLINE_REPO", "/home/john/wardline")
    authority = (
        Path(repo)
        / "tests"
        / "conformance"
        / "fixtures"
        / "wardline-vocabulary-descriptor.golden.yaml"
    )
    if not authority.exists():
        pytest.skip(
            f"wardline authority golden not found at {authority} — skipping Layer-2 drift "
            "recheck (set WARDLINE_REPO to enable)"
        )

    authority_bytes = authority.read_bytes()
    vendored_bytes = GOLDEN_PATH.read_bytes()
    assert authority_bytes == vendored_bytes, (
        f"the vendored golden has DRIFTED from the Wardline authority at {authority}; "
        "re-vendor BYTE-IDENTICAL (cmp must show no difference) and re-pin UPSTREAM_BLOB_SHA"
    )
