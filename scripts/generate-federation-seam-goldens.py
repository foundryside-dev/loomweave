#!/usr/bin/env python3
"""Generate Loomweave-owned federation seam fixtures from built artifacts."""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
LOOMWEAVE = ROOT / "target/debug/loomweave"
RUST_PLUGIN = ROOT / "target/debug/loomweave-rust-plugin"
PYTHON_PLUGIN = ROOT / "plugins/python/.venv/bin/loomweave-plugin-python"
FIXTURE_SOURCE = ROOT / "tests/fixtures/federation_classifier_python"
STABLE_INSTANCE_ID = "9bd7234e-6d44-4a38-9ae4-76f912a10221"
TAGS = ("cli-command", "entry-point", "http-route", "exported-api")
PRODUCTION_SEI_PREFIX = "loomweave:eid:"
NO_AUTH_TOKEN_ENV = "LOOMWEAVE_GOLDEN_NO_AUTH_UNSET"


def pretty(value: object) -> bytes:
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(pretty(value))


def write_digest(path: Path) -> None:
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    path.with_name(path.name + ".sha256").write_text(
        f"{digest}  {path.name}\n", encoding="ascii"
    )


def run(command: list[str], *, env: dict[str, str] | None = None) -> None:
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def write_frame(process: subprocess.Popen[bytes], message: dict[str, object]) -> None:
    assert process.stdin is not None
    body = json.dumps(message, separators=(",", ":")).encode()
    process.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    process.stdin.flush()


def read_frame(process: subprocess.Popen[bytes]) -> dict[str, Any]:
    assert process.stdout is not None
    headers: dict[str, str] = {}
    while True:
        line = process.stdout.readline()
        if not line:
            stderr = (
                process.stderr.read().decode(errors="replace") if process.stderr else ""
            )
            raise RuntimeError(f"MCP server exited while reading a frame: {stderr}")
        if line == b"\r\n":
            break
        name, value = line.decode("ascii").strip().split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(process.stdout.read(int(headers["content-length"])))


def normalize(value: object, *, run_id: str, project_root: Path) -> object:
    if isinstance(value, dict):
        return {
            key: normalize(item, run_id=run_id, project_root=project_root)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [
            normalize(item, run_id=run_id, project_root=project_root) for item in value
        ]
    if isinstance(value, str):
        if value == run_id:
            return "<run-id>"
        root = str(project_root)
        if value == root or value.startswith(root + os.sep):
            return "<fixture-root>" + value[len(root) :]
    return value


def normalized_sei(production_sei: str, locator: str) -> str:
    """Replace a verified production SEI with an unmistakable opaque placeholder."""
    suffix = production_sei.removeprefix(PRODUCTION_SEI_PREFIX)
    if (
        not production_sei.startswith(PRODUCTION_SEI_PREFIX)
        or len(suffix) != 32
        or any(character not in "0123456789abcdef" for character in suffix)
    ):
        raise ValueError(
            f"expected a real freshly minted production SEI, got {production_sei!r}"
        )
    locator_key = hashlib.sha256(locator.encode()).hexdigest()[:24]
    return f"normalized-sei:{locator_key}"


def hermetic_http_env() -> dict[str, str]:
    env = os.environ.copy()
    for name in (
        "WEFT_TOKEN",
        NO_AUTH_TOKEN_ENV,
        "LOOMWEAVE_GOLDEN_BEARER",
        "LOOMWEAVE_GOLDEN_HMAC",
    ):
        env.pop(name, None)
    return env


def mcp_classifications(project: Path, run_id: str) -> dict[str, object]:
    config = project / "loomweave.yaml"
    config.write_text(
        "version: 1\nserve:\n  http:\n    enabled: false\n", encoding="utf-8"
    )
    process = subprocess.Popen(
        [str(LOOMWEAVE), "serve", "--path", str(project)],
        cwd=ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        write_frame(
            process,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "federation-golden-generator",
                        "version": "1",
                    },
                },
            },
        )
        read_frame(process)
        responses: dict[str, object] = {}
        for request_id, tag in enumerate(TAGS, 2):
            write_frame(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "entity_tag_list",
                        "arguments": {"tag": tag, "limit": 200},
                    },
                },
            )
            wire = read_frame(process)
            text = wire["result"]["content"][0]["text"]
            envelope = json.loads(text)
            for entity in envelope["result"]["entities"]:
                entity["sei"] = normalized_sei(entity["sei"], entity["id"])
            responses[tag] = normalize(envelope, run_id=run_id, project_root=project)
        return responses
    finally:
        if process.stdin:
            process.stdin.close()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def http_json(url: str) -> dict[str, object]:
    last_error: Exception | None = None
    for _ in range(100):
        try:
            with urllib.request.urlopen(url, timeout=1) as response:  # noqa: S310 - fixed loopback URL
                return json.load(response)
        except (urllib.error.URLError, TimeoutError) as error:
            last_error = error
            time.sleep(0.02)
    raise RuntimeError(f"HTTP handler never became ready: {last_error}")


def http_response(
    url: str, *, headers: dict[str, str] | None = None
) -> dict[str, object]:
    request = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=2) as response:  # noqa: S310 - fixed loopback URL
            return {"status": response.status, "body": json.load(response)}
    except urllib.error.HTTPError as error:
        return {"status": error.code, "body": json.loads(error.read())}


def hmac_headers(
    secret: str,
    path: str,
    timestamp: int,
    nonce: str,
) -> dict[str, str]:
    body_sha = hashlib.sha256(b"").hexdigest()
    message = f"GET\n{path}\n{body_sha}\n{timestamp}\n{nonce}"
    signature = hmac.new(secret.encode(), message.encode(), hashlib.sha256).hexdigest()
    return {
        "X-Weft-Component": f"loomweave:{signature}",
        "X-Weft-Timestamp": str(timestamp),
        "X-Weft-Nonce": nonce,
    }


def http_handler_fixtures(
    project: Path, sei: str
) -> tuple[dict[str, object], dict[str, object]]:
    port = free_port()
    (project / "loomweave.yaml").write_text(
        f'version: 1\nserve:\n  http:\n    enabled: true\n    bind: "127.0.0.1:{port}"\n'
        f'    token_env: "{NO_AUTH_TOKEN_ENV}"\n',
        encoding="utf-8",
    )
    process = subprocess.Popen(
        [str(LOOMWEAVE), "serve", "--path", str(project)],
        cwd=ROOT,
        env=hermetic_http_env(),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        base = f"http://127.0.0.1:{port}"
        capabilities = http_json(base + "/api/v1/_capabilities")
        identity = http_json(base + f"/api/v1/identity/sei/{sei}")
        return capabilities, identity
    finally:
        if process.stdin:
            process.stdin.close()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def auth_handler_fixture(project: Path, sei: str) -> dict[str, object]:
    path = f"/api/v1/identity/sei/{sei}"
    secret = "federation-secret"

    def run_mode(
        config_key: str, env_name: str, cases: list[tuple[str, dict[str, str]]]
    ) -> dict[str, object]:
        port = free_port()
        (project / "loomweave.yaml").write_text(
            f'version: 1\nserve:\n  http:\n    enabled: true\n    bind: "127.0.0.1:{port}"\n'
            f'    {config_key}: "{env_name}"\n',
            encoding="utf-8",
        )
        env = hermetic_http_env()
        env.pop(env_name, None)
        env[env_name] = secret
        process = subprocess.Popen(
            [str(LOOMWEAVE), "serve", "--path", str(project)],
            cwd=ROOT,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            base = f"http://127.0.0.1:{port}"
            http_json(base + "/api/v1/_capabilities")
            return {
                name: http_response(base + path, headers=headers)
                for name, headers in cases
            }
        finally:
            if process.stdin:
                process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    bearer = run_mode(
        "token_env",
        "LOOMWEAVE_GOLDEN_BEARER",
        [
            ("missing", {}),
            ("malformed", {"Authorization": "Basic federation-secret"}),
            ("wrong", {"Authorization": "Bearer wrong-value"}),
            ("valid", {"Authorization": f"Bearer {secret}"}),
        ],
    )
    now = int(time.time())
    valid = hmac_headers(secret, path, now, "golden-valid-replay")
    hmac_cases = [
        ("missing", {}),
        (
            "malformed",
            {
                "X-Weft-Component": "loomweave:not-lowercase-hex",
                "X-Weft-Timestamp": "not-unix-seconds",
                "X-Weft-Nonce": "golden-malformed",
            },
        ),
        ("wrong", hmac_headers("wrong-value", path, now, "golden-wrong")),
        ("stale", hmac_headers(secret, path, now - 301, "golden-stale")),
        ("valid", valid),
        ("replayed", valid),
    ]
    hmac_matrix = run_mode(
        "identity_token_env",
        "LOOMWEAVE_GOLDEN_HMAC",
        hmac_cases,
    )
    for mode in (bearer, hmac_matrix):
        for response in mode.values():
            body = response["body"]
            if isinstance(body, dict) and body.get("current_locator"):
                body["sei"] = normalized_sei(
                    str(body["sei"]), str(body["current_locator"])
                )
    normalized_endpoint_sei = normalized_sei(
        sei, "python:function:command_alpha.command_alpha"
    )
    return {
        "endpoint": {
            "method": "GET",
            "path": f"/api/v1/identity/sei/{normalized_endpoint_sei}",
        },
        "bearer": bearer,
        "hmac": hmac_matrix,
    }


def artifact_metadata() -> dict[str, object]:
    python_manifest = tomllib.loads((ROOT / "plugins/python/plugin.toml").read_text())
    rust_manifest = tomllib.loads(
        (ROOT / "crates/loomweave-plugin-rust/plugin.toml").read_text()
    )
    cli_version = (
        subprocess.check_output([str(LOOMWEAVE), "--version"], text=True)
        .strip()
        .split()[-1]
    )
    return {
        "loomweave_cli": {
            "path": "<repo-root>/target/debug/loomweave",
            "version": cli_version,
        },
        "rust_plugin": {
            "path": "<repo-root>/target/debug/loomweave-rust-plugin",
            "version": rust_manifest["plugin"]["version"],
            "ontology_version": rust_manifest["ontology"]["ontology_version"],
        },
        "python_plugin": {
            "path": "<repo-root>/plugins/python/.venv/bin/loomweave-plugin-python",
            "version": python_manifest["plugin"]["version"],
            "ontology_version": python_manifest["ontology"]["ontology_version"],
        },
    }


def auth_fixture(handler_matrix: dict[str, object]) -> dict[str, object]:
    secret = b"federation-secret"
    body = b'{"locator":"python:function:demo.run"}'
    method = "POST"
    path = "/api/v1/identity/resolve?trace=1"
    timestamp = 1_900_000_000
    nonce = "nonce-vector-001"
    body_sha = hashlib.sha256(body).hexdigest()
    message = f"{method}\n{path}\n{body_sha}\n{timestamp}\n{nonce}"
    signature = hmac.new(secret, message.encode(), hashlib.sha256).hexdigest()
    return {
        "schema": "loomweave.http-auth.v1",
        "canonical_hmac": {
            "secret": "federation-secret",
            "method_input": "post",
            "method_on_wire": method,
            "path_and_query": path,
            "body_utf8": body.decode(),
            "body_sha256": body_sha,
            "timestamp": timestamp,
            "nonce": nonce,
            "signing_bytes_utf8": message,
            "component_signature": f"loomweave:{signature}",
        },
        "freshness": {
            "window_seconds": 300,
            "now": timestamp,
            "accepted_timestamps": [timestamp - 300, timestamp + 300],
            "rejected_timestamps": [timestamp - 301, timestamp + 301],
        },
        "redacted_unauthenticated": {
            "status": 401,
            "body": {"error": "authentication required", "code": "UNAUTHENTICATED"},
        },
        "handler_matrix": handler_matrix,
    }


def external_sqlite_fixture() -> dict[str, object]:
    surface = {
        "runs": ["id", "started_at", "completed_at", "stats", "status"],
        "entities": [
            "id",
            "plugin_id",
            "kind",
            "source_file_path",
            "source_byte_start",
            "source_byte_end",
            "source_line_start",
            "source_line_end",
            "properties",
            "content_hash",
        ],
        "entity_tags": ["entity_id", "plugin_id", "tag"],
        "sei_bindings": ["sei", "current_locator", "body_hash", "status"],
        "sei_lineage": [
            "sei",
            "event",
            "old_locator",
            "new_locator",
            "run_id",
            "recorded_at",
        ],
    }
    base = {
        "schema": "loomweave.external-sqlite.v1",
        "reason": None,
        "application_id": 0x4C4D5756,
        "min_user_version": 5,
        "max_user_version": 13,
        "legacy_application_id": False,
        "missing_surface": [],
    }
    return {
        "schema": "loomweave.external-sqlite-fixture.v1",
        "required_surface": surface,
        "reports": {
            "current": {**base, "status": "compatible", "user_version": 13},
            "older_supported": {**base, "status": "older_supported", "user_version": 5},
            "future": {
                **base,
                "status": "incompatible",
                "reason": "too_new",
                "user_version": 14,
            },
        },
    }


def main() -> None:
    for artifact in (LOOMWEAVE, RUST_PLUGIN, PYTHON_PLUGIN):
        if not artifact.is_file():
            raise SystemExit(f"missing explicit built artifact: {artifact}")

    env = os.environ.copy()
    env["PATH"] = os.pathsep.join(
        (str(LOOMWEAVE.parent), str(PYTHON_PLUGIN.parent), "/usr/bin", "/bin")
    )
    with tempfile.TemporaryDirectory(prefix="loomweave-federation-golden-") as raw_temp:
        project = Path(raw_temp)
        for source in FIXTURE_SOURCE.glob("*.py"):
            shutil.copy2(source, project / source.name)
        env["LOOMWEAVE_CODEX_CONFIG"] = str(project / "codex-config.toml")
        run([str(LOOMWEAVE), "install", "--path", str(project)], env=env)
        (project / ".weft/loomweave/instance_id").write_text(STABLE_INSTANCE_ID + "\n")
        run([str(LOOMWEAVE), "analyze", str(project)], env=env)

        db = project / ".weft/loomweave/loomweave.db"
        with sqlite3.connect(db) as connection:
            run_id, status, stats_raw = connection.execute(
                "select id, status, stats from runs order by started_at desc, id desc limit 1"
            ).fetchone()
            coverage = json.loads(stats_raw)["classifier_coverage"]
            counts = dict(
                connection.execute(
                    "select tag, count(distinct entity_id) from entity_tags "
                    "where tag in ('cli-command','entry-point','http-route','exported-api') group by tag"
                )
            )
            sei = connection.execute(
                "select sei from sei_bindings where current_locator = ? and status = 'alive'",
                ("python:function:command_alpha.command_alpha",),
            ).fetchone()[0]
        expected = {
            "cli-command": 5,
            "entry-point": 5,
            "http-route": 0,
            "exported-api": 0,
        }
        actual = {tag: int(counts.get(tag, 0)) for tag in TAGS}
        if status != "completed" or actual != expected:
            raise SystemExit(
                f"fixture analysis did not produce completed 5/5/0/0 proof: {status=} {actual=}"
            )

        responses = mcp_classifications(project, run_id)
        for tag, expected_count in expected.items():
            classification = responses[tag]["result"]["classification"]
            if classification["state"] != "supported" or not classification["complete"]:
                raise SystemExit(f"{tag} is not supported-complete: {classification}")
            if classification["matches"] != expected_count:
                raise SystemExit(f"{tag} match count drift: {classification}")

        capabilities_body, identity_body = http_handler_fixtures(project, sei)
        identity_body["sei"] = normalized_sei(
            identity_body["sei"], identity_body["current_locator"]
        )
        auth_matrix = auth_handler_fixture(project, sei)

    coverage_path = (
        ROOT
        / "crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json"
    )
    write_json(coverage_path, coverage)
    write_digest(coverage_path)

    classification_path = ROOT / "docs/federation/fixtures/classification.python.json"
    write_json(
        classification_path,
        {
            "schema": "loomweave.federation-classification-fixture.v1",
            "generated_with": artifact_metadata(),
            "normalization": [
                "analysis run UUID -> <run-id>",
                "temporary fixture root -> <fixture-root>",
                "checkout root in recorded artifact paths -> <repo-root>",
                "verified production SEIs whose mint input contains the run UUID -> normalized-sei:<locator-key> opaque placeholders (not production mints)",
                "run timestamps are omitted because neither contract contains them",
            ],
            "expected_matches": expected,
            "responses": responses,
        },
    )
    write_digest(classification_path)

    capabilities_path = ROOT / "docs/federation/fixtures/get-api-v1-capabilities.json"
    capabilities = json.loads(capabilities_path.read_text())
    capabilities["examples"][0]["response"]["body"] = capabilities_body
    write_json(capabilities_path, capabilities)
    write_digest(capabilities_path)

    identity_path = ROOT / "docs/federation/fixtures/identity-ownership-v1.golden.json"
    write_json(
        identity_path,
        {
            "schema": "loomweave.identity-ownership.v1",
            "local_instance_id": STABLE_INSTANCE_ID,
            "capabilities_ownership": {
                "api_version": capabilities_body["api_version"],
                "instance_id": capabilities_body["instance_id"],
            },
            "request": {
                "method": "GET",
                "path": f"/api/v1/identity/sei/{identity_body['sei']}",
            },
            "identity_response": identity_body,
            "join_allowed": True,
        },
    )
    write_digest(identity_path)

    auth_path = ROOT / "docs/federation/fixtures/loomweave-http-auth-v1.golden.json"
    write_json(auth_path, auth_fixture(auth_matrix))
    write_digest(auth_path)

    external_path = (
        ROOT / "docs/federation/fixtures/external-sqlite-compatibility-v1.json"
    )
    write_json(external_path, external_sqlite_fixture())
    write_digest(external_path)


if __name__ == "__main__":
    main()
