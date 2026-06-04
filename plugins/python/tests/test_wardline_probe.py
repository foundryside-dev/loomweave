"""Unit tests for the L8 Wardline probe (WP3 Task 6).

Each case stubs importlib and Path operations to simulate
the absent / in-range / out-of-range states without requiring
the real ``wardline`` package to be present or absent in the test
environment.
"""

from __future__ import annotations

import importlib.metadata
import io
from pathlib import Path
from typing import TYPE_CHECKING, Any

from clarion_plugin_python.wardline_probe import probe

if TYPE_CHECKING:
    import pytest


def _install_fake_probe_env(  # noqa: PLR0913
    monkeypatch: pytest.MonkeyPatch,
    *,
    installed: bool,
    vocab_exists: bool,
    vocab_valid_yaml: bool,
    version_via_metadata: str | None = None,
    version_via_import: Any = None,
) -> None:
    """Mock the importlib, Path, open, and yaml behaviors."""

    # 1. Mock find_spec
    def fake_find_spec(name: str) -> object | None:
        if name == "wardline" and installed:

            class FakeSpec:
                submodule_search_locations = ["/fake/wardline"]  # noqa: RUF012

            return FakeSpec()
        return None

    monkeypatch.setattr(
        "clarion_plugin_python.wardline_probe.importlib.util.find_spec",
        fake_find_spec,
    )

    # 2. Mock Path.is_file
    orig_is_file = Path.is_file

    def fake_is_file(self: Path) -> bool:
        if str(self) == "/fake/wardline/core/vocabulary.yaml":
            return vocab_exists
        return orig_is_file(self)

    monkeypatch.setattr("clarion_plugin_python.wardline_probe.Path.is_file", fake_is_file)

    # 3. Mock Path.open and yaml.safe_load
    orig_open = Path.open

    def fake_open(self: Path, *args: Any, **kwargs: Any) -> Any:
        if str(self) == "/fake/wardline/core/vocabulary.yaml":
            if vocab_valid_yaml:
                return io.StringIO("version: 1.0.0\nentries: []")
            return io.StringIO("unbalanced: [")
        return orig_open(self, *args, **kwargs)

    monkeypatch.setattr("clarion_plugin_python.wardline_probe.Path.open", fake_open)

    # 4. Mock importlib.metadata.version
    def fake_version(name: str) -> str:
        if name == "wardline" and version_via_metadata is not None:
            return version_via_metadata
        raise importlib.metadata.PackageNotFoundError

    monkeypatch.setattr(
        "clarion_plugin_python.wardline_probe.importlib.metadata.version",
        fake_version,
    )

    # 5. Mock importlib.import_module
    def fake_import(name: str) -> object:
        if name == "wardline":
            if version_via_import is None:
                msg = "no wardline"
                raise ImportError(msg)

            class FakeWardline:
                __version__ = version_via_import

            return FakeWardline()
        msg = f"unexpected import: {name}"
        raise ImportError(msg)

    monkeypatch.setattr(
        "clarion_plugin_python.wardline_probe.importlib.import_module",
        fake_import,
    )


def test_probe_absent_when_not_installed(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(
        monkeypatch, installed=False, vocab_exists=False, vocab_valid_yaml=False
    )
    assert probe("0.1.0", "0.2.0") == {"status": "absent"}


def test_probe_absent_when_vocab_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(monkeypatch, installed=True, vocab_exists=False, vocab_valid_yaml=False)
    assert probe("0.1.0", "0.2.0") == {"status": "absent"}


def test_probe_absent_when_vocab_invalid(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(monkeypatch, installed=True, vocab_exists=True, vocab_valid_yaml=False)
    assert probe("0.1.0", "0.2.0") == {"status": "absent"}


def test_probe_enabled_when_version_in_range_metadata(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata="0.1.5",
    )
    assert probe("0.1.0", "0.2.0") == {"status": "enabled", "version": "0.1.5"}


def test_probe_enabled_when_version_in_range_import_fallback(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata=None,
        version_via_import="0.1.5",
    )
    assert probe("0.1.0", "0.2.0") == {"status": "enabled", "version": "0.1.5"}


def test_probe_at_lower_bound_is_enabled(monkeypatch: pytest.MonkeyPatch) -> None:
    """Lower bound is inclusive."""
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata="0.1.0",
    )
    assert probe("0.1.0", "0.2.0") == {"status": "enabled", "version": "0.1.0"}


def test_probe_at_upper_bound_is_out_of_range(monkeypatch: pytest.MonkeyPatch) -> None:
    """Upper bound is exclusive."""
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata="0.2.0",
    )
    assert probe("0.1.0", "0.2.0") == {"status": "version_out_of_range", "version": "0.2.0"}


def test_probe_above_upper_bound_is_out_of_range(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata="0.3.0",
    )
    assert probe("0.1.0", "0.2.0") == {"status": "version_out_of_range", "version": "0.3.0"}


def test_probe_absent_when_version_attribute_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata=None,
        version_via_import=None,
    )
    assert probe("0.1.0", "0.2.0") == {"status": "absent"}


def test_probe_absent_when_version_is_not_a_string(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata=None,
        version_via_import=123,
    )
    assert probe("0.1.0", "0.2.0") == {"status": "absent"}


def test_probe_absent_when_version_is_not_valid_semver(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_fake_probe_env(
        monkeypatch,
        installed=True,
        vocab_exists=True,
        vocab_valid_yaml=True,
        version_via_metadata="not-a-version",
    )
    assert probe("0.1.0", "0.2.0") == {"status": "absent"}
