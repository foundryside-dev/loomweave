"""L8 Wardline REGISTRY import + version-pin probe (WP3 Task 6).

At ``initialize``, the Python plugin runs this probe to report its
Wardline integration state in the handshake's ``capabilities.wardline``
field. The probe is deliberately fail-soft: Wardline missing or
out-of-range is not an error — the plugin continues to extract entities,
just without the REGISTRY cross-check.

Three states, matching the WP3 doc §L8:

- ``{"status": "absent"}`` — Wardline package not installed (or
  ``wardline.core.registry`` not importable; or ``wardline.__version__``
  is missing / not a string).
- ``{"status": "enabled", "version": "X.Y.Z"}`` — installed and
  ``wardline.__version__`` is in the half-open range
  ``[min_version, max_version)``.
- ``{"status": "version_out_of_range", "version": "X.Y.Z"}`` —
  installed but version outside the declared range.

Sprint 1 does not consume REGISTRY; the probe only proves the import
works + the version-pin handshake is wired end-to-end. Full REGISTRY
joining is WP3-feature-complete scope (ADR-018).
"""

from __future__ import annotations

import importlib
import importlib.metadata
import importlib.util
from pathlib import Path
from typing import Any

from packaging.version import InvalidVersion, Version

_ABSENT: dict[str, Any] = {"status": "absent"}


def probe(min_version: str, max_version: str) -> dict[str, Any]:
    """Probe the Wardline package for presence and version compatibility."""
    try:
        # Locate the wardline package
        spec = importlib.util.find_spec("wardline")
        if spec is None or not spec.submodule_search_locations:
            return _ABSENT

        # Check if core/vocabulary.yaml exists and is readable
        package_dir = Path(spec.submodule_search_locations[0])
        vocab_path = package_dir / "core" / "vocabulary.yaml"
        if not vocab_path.is_file():
            return _ABSENT

        # Verify it can be loaded as valid YAML
        try:
            import yaml  # type: ignore[import-untyped]  # noqa: PLC0415

            with vocab_path.open(encoding="utf-8") as f:
                yaml.safe_load(f)
        except (ImportError, OSError, yaml.YAMLError):
            return _ABSENT

        # Extract the version
        raw_version = None
        try:
            raw_version = importlib.metadata.version("wardline")
        except importlib.metadata.PackageNotFoundError:
            try:
                wardline = importlib.import_module("wardline")
                raw_version = getattr(wardline, "__version__", None)
            except ImportError:
                return _ABSENT

        if not isinstance(raw_version, str):
            return _ABSENT

        try:
            version = Version(raw_version)
            low = Version(min_version)
            high = Version(max_version)
        except InvalidVersion:
            return _ABSENT

        if low <= version < high:
            return {"status": "enabled", "version": raw_version}
        return {"status": "version_out_of_range", "version": raw_version}  # noqa: TRY300

    except Exception:  # noqa: BLE001
        return _ABSENT
