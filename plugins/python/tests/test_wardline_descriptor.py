from __future__ import annotations

import builtins
from pathlib import Path
from typing import Any

import yaml

from loomweave_plugin_python.wardline_descriptor import (
    EXPECTED_DESCRIPTOR_VERSION,
    load_wardline_descriptor,
)

GENERIC_3_FIXTURE = (
    Path(__file__).parent / "fixtures" / "wardline-vocabulary-descriptor.generic-3.preview.yaml"
)


def _plant(tmp_path: Path, text: str) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True, exist_ok=True)
    descriptor.write_text(text, encoding="utf-8")


_DESCRIPTOR = """\
version: wardline-generic-2
entries:
- canonical_name: external_boundary
  group: 1
  attrs: {}
- canonical_name: trust_boundary
  group: 1
  attrs:
    _wardline_to_level: TaintState
- canonical_name: trusted
  group: 1
  attrs:
    _wardline_level: TaintState
"""


class _FakePackagePath:
    def __init__(self, path: Any) -> None:
        self._path = path

    def __str__(self) -> str:
        return "wardline/core/vocabulary.yaml"

    def locate(self) -> Any:
        return self._path


def test_project_descriptor_wins_over_package_descriptor(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    project_descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    project_descriptor.parent.mkdir(parents=True)
    project_descriptor.write_text(_DESCRIPTOR, encoding="utf-8")
    package_descriptor = tmp_path / "package-vocabulary.yaml"
    package_descriptor.write_text(
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-9"),
        encoding="utf-8",
    )
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda name: [_FakePackagePath(package_descriptor)] if name == "wardline" else None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "project"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION
    assert state.vocabulary is not None
    assert sorted(state.vocabulary.entries_by_name) == [
        "external_boundary",
        "trust_boundary",
        "trusted",
    ]


def test_weft_descriptor_location_is_read(tmp_path: Path, monkeypatch: Any) -> None:
    # ADR-046: the consolidated .weft/wardline/ location is read as a project
    # descriptor (preferred over the package descriptor).
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(_DESCRIPTOR, encoding="utf-8")
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda _name: None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "project"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION


def test_legacy_wardline_location_is_not_read(tmp_path: Path, monkeypatch: Any) -> None:
    # ADR-046 clean break: a descriptor at only the pre-consolidation .wardline/
    # path is NOT read. With no package descriptor either, the loader degrades to
    # absent — a loud signal of a mis-sequenced cutover, not a silent stale read.
    legacy = tmp_path / ".wardline" / "vocabulary.yaml"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(_DESCRIPTOR, encoding="utf-8")
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda _name: None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.reason == "not_found"
    assert state.vocabulary is None


def test_legacy_wardline_location_does_not_shadow_package(tmp_path: Path, monkeypatch: Any) -> None:
    # A legacy .wardline/ descriptor is ignored, so the package descriptor (the
    # next resolution rung) is what loads — the legacy file never wins.
    legacy = tmp_path / ".wardline" / "vocabulary.yaml"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-9"),
        encoding="utf-8",
    )
    package_descriptor = tmp_path / "package-vocabulary.yaml"
    package_descriptor.write_text(_DESCRIPTOR, encoding="utf-8")
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda name: [_FakePackagePath(package_descriptor)] if name == "wardline" else None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "package"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION


def test_package_descriptor_loads_without_importing_wardline(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    package_descriptor = tmp_path / "vocabulary.yaml"
    package_descriptor.write_text(_DESCRIPTOR, encoding="utf-8")

    real_import = builtins.__import__

    def fail_import(name: str, *args: Any, **kwargs: Any) -> object:
        if name == "wardline" or name.startswith("wardline."):
            msg = f"unexpected Wardline import: {name}"
            raise AssertionError(msg)
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda name: [_FakePackagePath(package_descriptor)] if name == "wardline" else None,
    )
    monkeypatch.setattr(
        "builtins.__import__",
        fail_import,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "package"
    assert state.vocabulary is not None
    assert state.vocabulary.confidence_basis == "descriptor"


def test_absent_descriptor_degrades_without_vocabulary(tmp_path: Path, monkeypatch: Any) -> None:
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda _name: None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.vocabulary is None
    assert state.reason == "not_found"


def test_invalid_descriptor_shape_degrades_to_absent(tmp_path: Path) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text("version: 3\nentries: nope\n", encoding="utf-8")

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.vocabulary is None
    assert state.reason == "invalid_descriptor"


def test_duplicate_canonical_names_degrade_to_absent(tmp_path: Path) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(
        """\
version: wardline-generic-2
entries:
- canonical_name: trusted
  group: 1
  attrs: {}
- canonical_name: trusted
  group: 1
  attrs: {}
""",
        encoding="utf-8",
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_version_skew_keeps_valid_vocabulary_with_degraded_confidence(tmp_path: Path) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-3"),
        encoding="utf-8",
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "version_skew"
    assert state.descriptor_version == "wardline-generic-3"
    assert state.expected_version == EXPECTED_DESCRIPTOR_VERSION
    assert state.vocabulary is not None
    assert state.vocabulary.confidence_basis == "descriptor_version_skew"


# ── (schema, version) pair acceptance ────────────────────────────────────────

# A minimal v2-schema body the facet cases perturb. Kept separate from the
# preview fixture so a malformed-facet case never edits the fixture the
# acceptance tests read.
_V2_BODY = """\
schema: wardline.vocabulary/v2
version: wardline-generic-3
entries:
- canonical_name: trusted
  group: 1
  attrs: {}
"""


def test_generic_3_descriptor_is_accepted_not_skew(tmp_path: Path) -> None:
    # Consumer-first dual-accept (wardline declaration-surface-v2 §13.1 item 1):
    # the (wardline.vocabulary/v2, wardline-generic-3) PAIR is accepted BEFORE
    # wardline can emit it — and acceptance means the v2 schema's `facets:`
    # section is READ, not tolerated as an unknown key (spec rev 9 §4.3, the
    # accept-and-read rule introduced at rev 5).
    _plant(tmp_path, GENERIC_3_FIXTURE.read_text(encoding="utf-8"))
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "enabled"
    assert state.descriptor_version == "wardline-generic-3"
    assert state.vocabulary is not None
    vocab = state.vocabulary
    assert vocab.schema == "wardline.vocabulary/v2"
    assert vocab.confidence_basis == "descriptor"
    # Exact equality, not a superset: a dropped v2 entry must red.
    assert sorted(vocab.entries_by_name) == ["external_boundary", "trust_boundary", "trusted"]
    assert {n: e.attrs for n, e in vocab.entries_by_name.items()} == {
        "external_boundary": {},
        "trust_boundary": {"_wardline_to_level": "TaintState"},
        "trusted": {"_wardline_level": "TaintState"},
    }
    # The facets: section is READ, not silently ignored — the S1 defect.
    assert sorted(vocab.facets_by_name) == ["audit_record"]
    assert vocab.facets_by_name["audit_record"].group == 3
    assert vocab.facets_by_name["audit_record"].attrs == {}
    # …and it is a FACET, never folded into the seeding-marker table.
    assert "audit_record" not in vocab.entries_by_name
    assert vocab.entry_for_decorator("audit_record") is None
    assert (
        vocab.facet_for_decorator("weft_markers.audit_record")
        is vocab.facets_by_name["audit_record"]
    )


def test_generic_3_with_v1_schema_is_pair_mismatch_skew(tmp_path: Path) -> None:
    # Version alone is NOT enough: generic-3 under the v1 schema is not an
    # accepted PAIR — degrade to skew exactly like an unknown version.
    _plant(tmp_path, _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-3"))
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "version_skew"
    assert state.descriptor_version == "wardline-generic-3"


def test_generic_9_is_still_version_skew(tmp_path: Path) -> None:
    _plant(tmp_path, _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-9"))
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "version_skew"
    assert state.descriptor_version == "wardline-generic-9"


def test_generic_2_under_v2_schema_is_pair_mismatch_skew(tmp_path: Path) -> None:
    # The other direction of the pair gate: the accepted version under an
    # unaccepted schema is skew too, so neither coordinate alone unlocks
    # acceptance.
    _plant(tmp_path, "schema: wardline.vocabulary/v2\n" + _DESCRIPTOR)
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "version_skew"
    assert state.descriptor_version == "wardline-generic-2"


def test_non_string_schema_degrades_to_absent(tmp_path: Path) -> None:
    _plant(tmp_path, "schema: 2\n" + _DESCRIPTOR)
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


# ── the v2 facets: section ───────────────────────────────────────────────────


def test_generic_3_facets_section_is_parsed(tmp_path: Path) -> None:
    _plant(tmp_path, GENERIC_3_FIXTURE.read_text(encoding="utf-8"))
    state = load_wardline_descriptor(tmp_path)
    assert state.vocabulary is not None
    facet = state.vocabulary.facets_by_name["audit_record"]
    assert facet.canonical_name == "audit_record"
    assert facet.group == 3
    assert facet.attrs == {}


def test_v1_descriptor_without_facets_key_parses_with_empty_facets(tmp_path: Path) -> None:
    _plant(tmp_path, _DESCRIPTOR)
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "enabled"
    assert state.vocabulary is not None
    assert state.vocabulary.schema == "wardline.vocabulary/v1"
    assert state.vocabulary.facets_by_name == {}


def test_facets_section_that_is_not_a_list_degrades_to_absent(tmp_path: Path) -> None:
    _plant(tmp_path, _V2_BODY + "facets: nope\n")
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_malformed_facet_entry_degrades_to_absent(tmp_path: Path) -> None:
    for index, facets in enumerate(
        (
            "facets:\n- nope\n",
            "facets:\n- group: 3\n",
            "facets:\n- canonical_name: audit_record\n",
            "facets:\n- canonical_name: 3\n  group: 3\n",
        ),
    ):
        target = tmp_path / f"case{index}"
        target.mkdir()
        _plant(target, _V2_BODY + facets)
        state = load_wardline_descriptor(target)
        assert state.status == "absent", facets
        assert state.reason == "invalid_descriptor", facets


def test_facet_carrying_attrs_degrades_to_absent(tmp_path: Path) -> None:
    # A facet seeds no taint, so attrs on a facet is a contract violation,
    # never an honoured trust marker.
    _plant(
        tmp_path,
        _V2_BODY + "facets:\n- canonical_name: audit_record\n  group: 3\n  attrs:\n    x: y\n",
    )
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_facet_with_explicit_empty_attrs_is_accepted(tmp_path: Path) -> None:
    # The other direction of the attrs rule: an explicit empty mapping is the
    # shape the producer may emit, and it must NOT be rejected.
    _plant(
        tmp_path,
        _V2_BODY + "facets:\n- canonical_name: audit_record\n  group: 3\n  attrs: {}\n",
    )
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "enabled"
    assert state.vocabulary is not None
    assert sorted(state.vocabulary.facets_by_name) == ["audit_record"]


def test_duplicate_facet_canonical_names_degrade_to_absent(tmp_path: Path) -> None:
    _plant(
        tmp_path,
        _V2_BODY
        + "facets:\n- canonical_name: audit_record\n  group: 3\n"
        + "- canonical_name: audit_record\n  group: 3\n",
    )
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_facet_colliding_with_entry_name_degrades_to_absent(tmp_path: Path) -> None:
    # `trusted` is an entry in _V2_BODY; a facet of the same name would make the
    # two lookups disagree about what the decorator means.
    _plant(tmp_path, _V2_BODY + "facets:\n- canonical_name: trusted\n  group: 3\n")
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_facets_section_under_v1_schema_degrades_to_absent(tmp_path: Path) -> None:
    # v1's shape is known exactly; honouring a section the declared schema does
    # not have is the fail-open this reader exists to avoid.
    _plant(tmp_path, _DESCRIPTOR + "facets:\n- canonical_name: audit_record\n  group: 3\n")
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_v1_schema_facets_under_an_unaccepted_version_fails_closed(tmp_path: Path) -> None:
    # Composition of both violations: an unaccepted PAIR and a v1-schema facets
    # section. Parse failure must beat pair mismatch — `absent`, never the
    # softer `version_skew` — so a later refactor that hoists the gate ahead of
    # the parse cannot silently downgrade a worse descriptor to a milder status.
    _plant(
        tmp_path,
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-3")
        + "facets:\n- canonical_name: audit_record\n  group: 3\n",
    )
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_version_skew_preserves_parsed_facets(tmp_path: Path) -> None:
    # The dataclasses.replace regression guard: a hand-copied field-by-field
    # skew rebuild would drop facets_by_name (and schema) silently.
    _plant(
        tmp_path,
        _V2_BODY.replace("wardline-generic-3", "wardline-generic-9")
        + "facets:\n- canonical_name: audit_record\n  group: 3\n",
    )
    state = load_wardline_descriptor(tmp_path)
    assert state.status == "version_skew"
    assert state.vocabulary is not None
    assert state.vocabulary.schema == "wardline.vocabulary/v2"
    assert state.vocabulary.confidence_basis == "descriptor_version_skew"
    assert sorted(state.vocabulary.facets_by_name) == ["audit_record"]
    assert state.vocabulary.facets_by_name["audit_record"].group == 3


def test_generic_3_preview_fixture_declares_only_known_sections() -> None:
    # Tripwire: when the producer adds a section this reader has not learned,
    # this reds instead of the section being silently ignored.
    document = yaml.safe_load(GENERIC_3_FIXTURE.read_text(encoding="utf-8"))
    assert set(document) == {"schema", "version", "entries", "facets"}
