#!/usr/bin/env python3
"""Guard Python plugin ontology_version lockstep.

The Python plugin declares the ontology version in two places:

* plugins/python/plugin.toml, consumed by the Rust host during discovery.
* clarion_plugin_python.server.ONTOLOGY_VERSION, returned during initialize.

Those values are intentionally duplicated so the plugin can answer the JSON-RPC
handshake without parsing its installed manifest at runtime. This CI guard keeps
the duplication mechanical.
"""

from __future__ import annotations

import argparse
import ast
import sys
import tempfile
import tomllib
from pathlib import Path

DEFAULT_MANIFEST = Path("plugins/python/plugin.toml")
DEFAULT_SERVER = Path("plugins/python/src/clarion_plugin_python/server.py")


class CheckError(Exception):
    """Raised when the ontology-version guard fails."""


def manifest_ontology_version(manifest_path: Path) -> str:
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    try:
        version = manifest["ontology"]["ontology_version"]
    except KeyError as exc:
        raise CheckError(f"{manifest_path} is missing [ontology].ontology_version") from exc
    if not isinstance(version, str) or not version.strip():
        raise CheckError(f"{manifest_path} has invalid ontology_version {version!r}")
    return version


def server_ontology_version(server_path: Path) -> str:
    module = ast.parse(server_path.read_text(encoding="utf-8"), filename=str(server_path))
    for node in module.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "ONTOLOGY_VERSION"
            for target in node.targets
        ):
            if isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
                return node.value.value
            raise CheckError(f"{server_path}: ONTOLOGY_VERSION must be a string literal")
        if (
            isinstance(node, ast.AnnAssign)
            and isinstance(node.target, ast.Name)
            and node.target.id == "ONTOLOGY_VERSION"
        ):
            if isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
                return node.value.value
            raise CheckError(f"{server_path}: ONTOLOGY_VERSION must be a string literal")
    raise CheckError(f"{server_path} does not define ONTOLOGY_VERSION")


def check(manifest_path: Path, server_path: Path) -> str:
    manifest_version = manifest_ontology_version(manifest_path)
    server_version = server_ontology_version(server_path)
    if manifest_version != server_version:
        raise CheckError(
            "Python plugin ontology_version drift: "
            f"{manifest_path} has {manifest_version!r}, "
            f"{server_path} has {server_version!r}"
        )
    return manifest_version


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        manifest = root / "plugin.toml"
        server = root / "server.py"

        write(manifest, '[ontology]\nontology_version = "1.2.3"\n')
        write(server, 'ONTOLOGY_VERSION = "1.2.3"\n')
        assert check(manifest, server) == "1.2.3"

        write(server, 'ONTOLOGY_VERSION = "1.2.4"\n')
        try:
            check(manifest, server)
        except CheckError as exc:
            if "ontology_version drift" not in str(exc):
                raise
        else:
            raise CheckError("self-test expected ontology_version mismatch to fail")

    print("Python ontology_version guard self-test passed")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Check Python plugin ontology_version lockstep")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--server", type=Path, default=DEFAULT_SERVER)
    parser.add_argument("--self-test", action="store_true", help="run built-in guard tests")
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            run_self_test()
        else:
            version = check(args.manifest, args.server)
            print(f"Python plugin ontology_version matches: {version}")
    except CheckError as exc:
        print(f"Python ontology_version guard failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
