#!/usr/bin/env python3
"""Check GitHub-side release governance before cutting a Clarion tag.

The release workflow can prove build and artifact integrity once it runs, but
some 1.0 release controls live outside the repository tree: branch protection,
repository rulesets, and the Actions source policy. This guard queries the live
GitHub REST API and fails when those controls are still permissive.

Exit codes:
    0  release governance is non-permissive enough for the v1.0 tag gate
    1  the live settings are too permissive
    2  usage, authentication, or API access error
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any


class CheckError(Exception):
    """Raised when release governance is too permissive."""


class UsageError(Exception):
    """Raised when the guard cannot query GitHub correctly."""


@dataclass(frozen=True)
class ApiResponse:
    status: int
    payload: Any


class GitHubClient:
    """Tiny GitHub REST client using only the Python standard library."""

    def __init__(self, token: str, api_base: str = "https://api.github.com") -> None:
        self.token = token
        self.api_base = api_base.rstrip("/")

    def get(self, path: str) -> ApiResponse:
        url = f"{self.api_base}{path}"
        request = urllib.request.Request(
            url,
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self.token}",
                "User-Agent": "clarion-release-governance-check",
                "X-GitHub-Api-Version": "2022-11-28",
            },
            method="GET",
        )
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                body = response.read().decode("utf-8")
                return ApiResponse(response.status, json.loads(body) if body else None)
        except urllib.error.HTTPError as exc:
            body = exc.read().decode("utf-8")
            try:
                payload: Any = json.loads(body) if body else None
            except json.JSONDecodeError:
                payload = body
            return ApiResponse(exc.code, payload)
        except OSError as exc:
            raise UsageError(f"GitHub API request failed: {exc}") from exc


def require_mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise UsageError(f"{label} returned unexpected payload: {value!r}")
    return value


def require_list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise UsageError(f"{label} returned unexpected payload: {value!r}")
    return value


def api_message(payload: Any) -> str:
    if isinstance(payload, dict) and isinstance(payload.get("message"), str):
        return payload["message"]
    return repr(payload)


def branch_has_protection(
    request_json: Callable[[str], ApiResponse],
    repository: str,
    branch: str,
) -> bool:
    response = request_json(f"/repos/{repository}/branches/{branch}/protection")
    if response.status == 200:
        require_mapping(response.payload, "branch protection")
        return True
    if response.status == 404:
        return False
    raise UsageError(
        f"cannot inspect branch protection for {repository}@{branch}: "
        f"HTTP {response.status} {api_message(response.payload)}"
    )


def repository_rulesets(
    request_json: Callable[[str], ApiResponse],
    repository: str,
) -> list[dict[str, Any]]:
    response = request_json(f"/repos/{repository}/rulesets")
    if response.status != 200:
        raise UsageError(
            f"cannot inspect repository rulesets for {repository}: "
            f"HTTP {response.status} {api_message(response.payload)}"
        )
    rulesets = require_list(response.payload, "repository rulesets")
    return [require_mapping(item, "repository ruleset") for item in rulesets]


def actions_permissions(
    request_json: Callable[[str], ApiResponse],
    repository: str,
) -> dict[str, Any]:
    response = request_json(f"/repos/{repository}/actions/permissions")
    if response.status != 200:
        raise UsageError(
            f"cannot inspect Actions permissions for {repository}: "
            f"HTTP {response.status} {api_message(response.payload)}"
        )
    return require_mapping(response.payload, "Actions permissions")


def check_governance(
    request_json: Callable[[str], ApiResponse],
    repository: str,
    branch: str,
) -> list[str]:
    failures: list[str] = []
    notes: list[str] = []

    protected = branch_has_protection(request_json, repository, branch)
    rulesets = repository_rulesets(request_json, repository)
    active_rulesets = [item for item in rulesets if item.get("enforcement") != "disabled"]
    if protected:
        notes.append(f"{branch}: branch protection is enabled")
    if active_rulesets:
        notes.append(f"{repository}: {len(active_rulesets)} active repository ruleset(s)")
    if not protected and not active_rulesets:
        failures.append(
            f"{branch}: no branch protection and no active repository rulesets; "
            "tag provenance can bypass the reviewed PR path"
        )

    permissions = actions_permissions(request_json, repository)
    if permissions.get("enabled") is not True:
        failures.append("GitHub Actions are not enabled for the repository")
    allowed_actions = permissions.get("allowed_actions")
    sha_pinning_required = permissions.get("sha_pinning_required")
    if allowed_actions == "all" and sha_pinning_required is not True:
        failures.append(
            "Actions source policy is permissive: allowed_actions=all and "
            "sha_pinning_required is not true"
        )
    else:
        notes.append(
            "Actions source policy is constrained "
            f"(allowed_actions={allowed_actions!r}, "
            f"sha_pinning_required={sha_pinning_required!r})"
        )

    if failures:
        raise CheckError("\n".join(failures))
    return notes


def run_self_test() -> None:
    responses: dict[str, ApiResponse] = {
        "/repos/acme/clarion/branches/main/protection": ApiResponse(
            404, {"message": "Branch not protected"}
        ),
        "/repos/acme/clarion/rulesets": ApiResponse(200, []),
        "/repos/acme/clarion/actions/permissions": ApiResponse(
            200,
            {
                "enabled": True,
                "allowed_actions": "all",
                "sha_pinning_required": False,
            },
        ),
    }

    def fake_get(path: str) -> ApiResponse:
        return responses[path]

    try:
        check_governance(fake_get, "acme/clarion", "main")
    except CheckError as exc:
        message = str(exc)
        assert "no branch protection" in message
        assert "allowed_actions=all" in message
    else:
        raise AssertionError("permissive fixture should fail")

    responses["/repos/acme/clarion/rulesets"] = ApiResponse(
        200, [{"name": "main release gate", "enforcement": "active"}]
    )
    responses["/repos/acme/clarion/actions/permissions"] = ApiResponse(
        200,
        {
            "enabled": True,
            "allowed_actions": "selected",
            "sha_pinning_required": True,
        },
    )
    notes = check_governance(fake_get, "acme/clarion", "main")
    assert any("active repository ruleset" in note for note in notes)

    print("GitHub release governance guard self-test passed")


def resolve_token() -> str | None:
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        return token
    proc = subprocess.run(
        ["gh", "auth", "token"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    if proc.returncode == 0 and proc.stdout.strip():
        return proc.stdout.strip()
    return None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repository",
        default=os.environ.get("GITHUB_REPOSITORY"),
        help="owner/repo to inspect; defaults to GITHUB_REPOSITORY",
    )
    parser.add_argument("--branch", default="main", help="release branch to inspect")
    parser.add_argument(
        "--api-base",
        default=os.environ.get("GITHUB_API_URL", "https://api.github.com"),
        help="GitHub API base URL",
    )
    parser.add_argument("--self-test", action="store_true", help="run built-in tests")
    args = parser.parse_args(argv)

    if args.self_test:
        run_self_test()
        return 0

    token = resolve_token()
    if not args.repository:
        print(
            "check-github-release-governance: --repository or GITHUB_REPOSITORY is required",
            file=sys.stderr,
        )
        return 2
    if not token:
        print(
            "check-github-release-governance: GITHUB_TOKEN or GH_TOKEN is required",
            file=sys.stderr,
        )
        return 2

    client = GitHubClient(token=token, api_base=args.api_base)
    try:
        notes = check_governance(client.get, args.repository, args.branch)
    except CheckError as exc:
        print("GitHub release governance guard failed:", file=sys.stderr)
        print(str(exc), file=sys.stderr)
        return 1
    except UsageError as exc:
        print(f"GitHub release governance guard could not run: {exc}", file=sys.stderr)
        return 2

    for note in notes:
        print(f"ok: {note}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
