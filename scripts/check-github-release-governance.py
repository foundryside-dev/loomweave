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
import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Any


class CheckError(Exception):
    """Raised when release governance is too permissive."""


class UsageError(Exception):
    """Raised when the guard cannot query GitHub correctly."""


FULL_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
USES_RE = re.compile(r"^\s*(?:-\s*)?uses:\s*(?P<target>\S+)\s*$")
JOB_RE = re.compile(r"^  (?P<name>[A-Za-z0-9_-]+):\s*(?:#.*)?$")
REQUIRED_STATUS_CHECKS = frozenset(
    {
        "Rust",
        "Rust (aarch64-apple-darwin)",
        "Python plugin",
        "Sprint 1 walking skeleton (end-to-end)",
    }
)
REQUIRED_GOVERNANCE_DOC_SNIPPETS = frozenset(
    {
        "active repository ruleset protects `refs/tags/v*`",
        '"target": "tag"',
        '"include": ["refs/tags/v*"]',
        "Pull-request flow is required before changes reach `main`.",
        "Workflow action references stay pinned to full commit SHAs.",
    }
)


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


def branch_protection(
    request_json: Callable[[str], ApiResponse],
    repository: str,
    branch: str,
) -> dict[str, Any] | None:
    response = request_json(f"/repos/{repository}/branches/{branch}/protection")
    if response.status == 200:
        return require_mapping(response.payload, "branch protection")
    if response.status == 404:
        return None
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


def repository_ruleset_detail(
    request_json: Callable[[str], ApiResponse],
    repository: str,
    ruleset: dict[str, Any],
) -> dict[str, Any]:
    ruleset_id = ruleset.get("id")
    if not isinstance(ruleset_id, int):
        return ruleset
    response = request_json(f"/repos/{repository}/rulesets/{ruleset_id}")
    if response.status != 200:
        raise UsageError(
            f"cannot inspect repository ruleset {ruleset_id} for {repository}: "
            f"HTTP {response.status} {api_message(response.payload)}"
        )
    return require_mapping(response.payload, "repository ruleset detail")


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


def branch_pattern_matches(pattern: str, branch: str) -> bool:
    if pattern in {branch, f"refs/heads/{branch}", "~DEFAULT_BRANCH"}:
        return True
    normalized = pattern.removeprefix("refs/heads/")
    return fnmatchcase(branch, normalized)


def ruleset_targets_branch(ruleset: dict[str, Any], branch: str) -> bool:
    target = ruleset.get("target")
    if target not in {None, "branch"}:
        return False

    conditions = ruleset.get("conditions")
    if not isinstance(conditions, dict):
        return True
    ref_name = conditions.get("ref_name")
    if not isinstance(ref_name, dict):
        return True

    exclude = ref_name.get("exclude")
    if isinstance(exclude, list) and any(
        isinstance(pattern, str) and branch_pattern_matches(pattern, branch)
        for pattern in exclude
    ):
        return False

    include = ref_name.get("include")
    if not isinstance(include, list) or not include:
        return True
    return any(
        isinstance(pattern, str) and branch_pattern_matches(pattern, branch)
        for pattern in include
    )


def ruleset_targets_tag(ruleset: dict[str, Any], tag_pattern: str) -> bool:
    """True when an active tag ruleset's include patterns cover ``tag_pattern``.

    GitHub tag rulesets carry ``target == "tag"`` and condition their
    ``ref_name`` on ``refs/tags/`` globs. We accept a ruleset if it either
    has no narrowing ``include`` list (covers all tags) or names an include
    pattern that the desired ``tag_pattern`` (e.g. ``refs/tags/v*``) satisfies.
    """
    if ruleset.get("target") != "tag":
        return False

    conditions = ruleset.get("conditions")
    if not isinstance(conditions, dict):
        return True
    ref_name = conditions.get("ref_name")
    if not isinstance(ref_name, dict):
        return True

    include = ref_name.get("include")
    if not isinstance(include, list) or not include:
        return True
    # The desired pattern is itself a glob (refs/tags/v*); treat an include
    # entry as covering it when the strings match, when the include is the
    # all-tags wildcard, or when the include glob matches the literal prefix
    # of the desired pattern.
    desired = tag_pattern.removeprefix("refs/tags/")
    for pattern in include:
        if not isinstance(pattern, str):
            continue
        normalized = pattern.removeprefix("refs/tags/")
        if pattern in {tag_pattern, "~ALL"} or normalized in {desired, "*"}:
            return True
        # An include like `v*` matches the concrete tag families we cut.
        if fnmatchcase("v1.0.0", normalized) or fnmatchcase(desired, normalized):
            return True
    return False


def branch_protection_status_checks(protection: dict[str, Any]) -> set[str]:
    required = protection.get("required_status_checks")
    if not isinstance(required, dict):
        return set()

    contexts = {
        context
        for context in required.get("contexts", [])
        if isinstance(context, str)
    }
    checks = {
        check.get("context")
        for check in required.get("checks", [])
        if isinstance(check, dict) and isinstance(check.get("context"), str)
    }
    return contexts | checks


def ruleset_status_checks(ruleset: dict[str, Any]) -> set[str]:
    checks: set[str] = set()
    rules = ruleset.get("rules")
    if not isinstance(rules, list):
        return checks

    for rule in rules:
        if not isinstance(rule, dict) or rule.get("type") != "required_status_checks":
            continue
        parameters = rule.get("parameters")
        if not isinstance(parameters, dict):
            continue
        for check in parameters.get("required_status_checks", []):
            if isinstance(check, dict) and isinstance(check.get("context"), str):
                checks.add(check["context"])
            elif isinstance(check, str):
                checks.add(check)
    return checks


def ruleset_requires_pull_request(ruleset: dict[str, Any]) -> bool:
    rules = ruleset.get("rules")
    if not isinstance(rules, list):
        return False
    return any(isinstance(rule, dict) and rule.get("type") == "pull_request" for rule in rules)


def protection_requires_pull_request(protection: dict[str, Any]) -> bool:
    return isinstance(protection.get("required_pull_request_reviews"), dict)


def missing_required_status_checks(actual: set[str]) -> set[str]:
    return REQUIRED_STATUS_CHECKS - actual


def check_governance(
    request_json: Callable[[str], ApiResponse],
    repository: str,
    branch: str,
) -> list[str]:
    failures: list[str] = []
    notes: list[str] = []

    protection = branch_protection(request_json, repository, branch)
    rule_summaries = repository_rulesets(request_json, repository)
    active_rulesets = [
        repository_ruleset_detail(request_json, repository, item)
        for item in rule_summaries
        if item.get("enforcement") == "active" and ruleset_targets_branch(item, branch)
    ]

    protected_path_ok = False
    if protection is not None:
        missing_checks = missing_required_status_checks(branch_protection_status_checks(protection))
        if missing_checks:
            failures.append(
                f"{branch}: branch protection is missing required CI checks: "
                f"{', '.join(sorted(missing_checks))}"
            )
        elif not protection_requires_pull_request(protection):
            failures.append(
                f"{branch}: branch protection does not require pull-request review flow; "
                "direct pushes can bypass the release PR path"
            )
        else:
            protected_path_ok = True
            notes.append(
                f"{branch}: branch protection requires pull-request flow and "
                f"{len(REQUIRED_STATUS_CHECKS)} release CI checks"
            )

    ruleset_path_ok = False
    for ruleset in active_rulesets:
        name = ruleset.get("name", "<unnamed ruleset>")
        if not isinstance(name, str):
            name = "<unnamed ruleset>"
        missing_checks = missing_required_status_checks(ruleset_status_checks(ruleset))
        has_pr_rule = ruleset_requires_pull_request(ruleset)
        if not missing_checks and has_pr_rule:
            ruleset_path_ok = True
            notes.append(
                f"{repository}: active ruleset {name!r} requires pull-request flow and "
                f"{len(REQUIRED_STATUS_CHECKS)} release CI checks"
            )

    if active_rulesets and not ruleset_path_ok:
        failures.append(
            f"{repository}: active repository rulesets targeting {branch} do not require "
            "both pull-request flow and the release CI checks"
        )
    if protection is None and not active_rulesets:
        failures.append(
            f"{branch}: no branch protection and no active repository rulesets; "
            "tag provenance can bypass the reviewed PR path"
        )
    elif not protected_path_ok and not ruleset_path_ok:
        failures.append(
            f"{branch}: no branch protection or ruleset currently proves the reviewed "
            "release PR path"
        )

    # Tag protection (GOV-02): an active ruleset must target `refs/tags/v*` so
    # that a tag-push cannot point a release tag at an arbitrary commit. The
    # legacy `tags/protection` endpoint is deprecated; a tag ruleset is the
    # modern mechanism. We assert on the ruleset summaries (no detail fetch is
    # needed for a presence check).
    tag_pattern = "refs/tags/v*"
    active_tag_rulesets = [
        item.get("name", "<unnamed ruleset>")
        for item in rule_summaries
        if item.get("enforcement") == "active" and ruleset_targets_tag(item, tag_pattern)
    ]
    if active_tag_rulesets:
        names = ", ".join(
            name if isinstance(name, str) else "<unnamed ruleset>"
            for name in active_tag_rulesets
        )
        notes.append(
            f"{repository}: active tag ruleset(s) protect {tag_pattern} ({names})"
        )
    else:
        failures.append(
            f"{repository}: no active repository ruleset protects {tag_pattern}; "
            "a tag-push can point a release tag at an arbitrary commit"
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


def check_workflow_action_pins(repo_root: Path) -> list[str]:
    workflow_dir = repo_root / ".github" / "workflows"
    if not workflow_dir.is_dir():
        raise UsageError(f"workflow directory not found: {workflow_dir}")

    failures: list[str] = []
    checked = 0
    for workflow in sorted([*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")]):
        for line_number, line in enumerate(workflow.read_text(encoding="utf-8").splitlines(), 1):
            match = USES_RE.match(line)
            if match is None:
                continue
            target = match.group("target")
            if target.startswith(("./", "docker://")):
                continue
            if "@" not in target:
                failures.append(f"{workflow}:{line_number}: external action is not pinned: {target}")
                continue
            ref = target.rsplit("@", maxsplit=1)[1]
            checked += 1
            if not FULL_SHA_RE.fullmatch(ref):
                failures.append(
                    f"{workflow}:{line_number}: action ref is not a full commit SHA: {target}"
                )

    if failures:
        raise CheckError("\n".join(failures))
    return [f"workflow action refs are full-length commit SHAs ({checked} uses entries checked)"]


def check_dependabot_github_actions_updates(repo_root: Path) -> list[str]:
    config = repo_root / ".github" / "dependabot.yml"
    if not config.is_file():
        raise CheckError(f"{config}: Dependabot config is missing")

    current: dict[str, str] = {}
    entries: list[dict[str, str]] = []
    for raw_line in config.read_text(encoding="utf-8").splitlines():
        stripped = raw_line.strip()
        if stripped.startswith("- package-ecosystem:"):
            if current:
                entries.append(current)
            current = {"package-ecosystem": stripped.split(":", maxsplit=1)[1].strip(" '\"")}
        elif current and stripped.startswith("directory:"):
            current["directory"] = stripped.split(":", maxsplit=1)[1].strip(" '\"")
        elif current and stripped.startswith("interval:"):
            current["interval"] = stripped.split(":", maxsplit=1)[1].strip(" '\"")
    if current:
        entries.append(current)

    for entry in entries:
        if entry.get("package-ecosystem") == "github-actions" and entry.get("directory") == "/":
            interval = entry.get("interval")
            if not interval:
                raise CheckError(f"{config}: github-actions Dependabot entry has no schedule interval")
            return [f"Dependabot watches GitHub Actions pins on / ({interval})"]

    raise CheckError(
        f"{config}: missing package-ecosystem=github-actions entry for directory=/; "
        "pinned workflow actions need a scheduled update path"
    )


def release_workflow_job_blocks(workflow: Path) -> dict[str, list[str]]:
    jobs: dict[str, list[str]] = {}
    current_job: str | None = None
    in_jobs = False

    for raw_line in workflow.read_text(encoding="utf-8").splitlines():
        if raw_line == "jobs:":
            in_jobs = True
            current_job = None
            continue
        if in_jobs and raw_line and not raw_line.startswith((" ", "#")):
            break
        if not in_jobs:
            continue

        match = JOB_RE.match(raw_line)
        if match is not None:
            current_job = match.group("name")
            jobs[current_job] = []
            continue
        if current_job is not None:
            jobs[current_job].append(raw_line)

    return jobs


def release_workflow_needs(job_lines: list[str]) -> set[str]:
    needs: set[str] = set()
    in_needs_block = False
    for line in job_lines:
        stripped = line.strip()
        if stripped.startswith("needs:"):
            in_needs_block = True
            value = stripped.split(":", maxsplit=1)[1].strip()
            if value.startswith("[") and value.endswith("]"):
                needs.update(
                    item.strip(" '\"") for item in value[1:-1].split(",") if item.strip()
                )
            elif value:
                needs.add(value.strip(" '\""))
            continue
        if in_needs_block:
            if stripped.startswith("- "):
                needs.add(stripped[2:].strip(" '\""))
                continue
            if line.startswith("    ") and not line.startswith("      "):
                in_needs_block = False
    return needs


def check_release_workflow_governance_gate(repo_root: Path) -> list[str]:
    workflow = repo_root / ".github" / "workflows" / "release.yml"
    if not workflow.is_file():
        raise CheckError(f"{workflow}: release workflow is missing")

    jobs = release_workflow_job_blocks(workflow)
    failures: list[str] = []
    if "release-governance" not in jobs:
        failures.append(f"{workflow}: missing release-governance job")
    else:
        governance_needs = release_workflow_needs(jobs["release-governance"])
        if "verify" not in governance_needs:
            failures.append(
                f"{workflow}: release-governance must need verify "
                "before using release governance secrets"
            )

    for job_name in ("build-rust", "build-plugin"):
        if job_name not in jobs:
            failures.append(f"{workflow}: missing {job_name} job")
            continue
        needs = release_workflow_needs(jobs[job_name])
        if "release-governance" not in needs:
            failures.append(
                f"{workflow}: {job_name} must need release-governance before release artifacts build"
            )

    if failures:
        raise CheckError("\n".join(failures))
    return ["release workflow build jobs require the GitHub governance gate"]


def check_governance_doc_lockstep(repo_root: Path) -> list[str]:
    doc = repo_root / "docs" / "operator" / "v1.0-release-governance.md"
    if not doc.is_file():
        raise CheckError(f"{doc}: release governance operator doc is missing")
    text = doc.read_text(encoding="utf-8")
    missing = sorted(
        snippet
        for snippet in REQUIRED_GOVERNANCE_DOC_SNIPPETS
        if snippet not in text
    )
    if missing:
        raise CheckError(
            f"{doc}: missing documented release governance control(s): "
            + ", ".join(repr(item) for item in missing)
        )
    return ["release governance operator doc covers every required live control"]


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

    responses["/repos/acme/clarion/actions/permissions"] = ApiResponse(
        200,
        {
            "enabled": True,
            "allowed_actions": "selected",
            "sha_pinning_required": True,
        },
    )
    responses["/repos/acme/clarion/rulesets"] = ApiResponse(
        200,
        [
            {
                "id": 42,
                "name": "weak release gate",
                "target": "branch",
                "enforcement": "active",
                "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            }
        ],
    )
    responses["/repos/acme/clarion/rulesets/42"] = ApiResponse(
        200,
        {
            "id": 42,
            "name": "weak release gate",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            "rules": [{"type": "pull_request", "parameters": {}}],
        },
    )
    try:
        check_governance(fake_get, "acme/clarion", "main")
    except CheckError as exc:
        assert "do not require both pull-request flow and the release CI checks" in str(exc)
    else:
        raise AssertionError("weak ruleset fixture should fail")

    responses["/repos/acme/clarion/rulesets"] = ApiResponse(
        200,
        [
            {
                "id": 44,
                "name": "evaluating release gate",
                "target": "branch",
                "enforcement": "evaluate",
                "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            }
        ],
    )
    responses["/repos/acme/clarion/rulesets/44"] = ApiResponse(
        200,
        {
            "id": 44,
            "name": "evaluating release gate",
            "target": "branch",
            "enforcement": "evaluate",
            "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            "rules": [
                {"type": "pull_request", "parameters": {"required_approving_review_count": 0}},
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "required_status_checks": [
                            {"context": "Rust"},
                            {"context": "Rust (aarch64-apple-darwin)"},
                            {"context": "Python plugin"},
                            {"context": "Sprint 1 walking skeleton (end-to-end)"},
                        ]
                    },
                },
            ],
        },
    )
    try:
        check_governance(fake_get, "acme/clarion", "main")
    except CheckError as exc:
        assert "no branch protection" in str(exc)
    else:
        raise AssertionError("evaluate-mode ruleset fixture should fail")

    responses["/repos/acme/clarion/rulesets"] = ApiResponse(
        200,
        [
            {
                "id": 43,
                "name": "main release gate",
                "target": "branch",
                "enforcement": "active",
                "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            },
            {
                "id": 50,
                "name": "release tag gate",
                "target": "tag",
                "enforcement": "active",
                "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
            },
        ],
    )
    responses["/repos/acme/clarion/rulesets/43"] = ApiResponse(
        200,
        {
            "id": 43,
            "name": "main release gate",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            "rules": [
                {"type": "pull_request", "parameters": {"required_approving_review_count": 0}},
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "required_status_checks": [
                            {"context": "Rust"},
                            {"context": "Rust (aarch64-apple-darwin)"},
                            {"context": "Python plugin"},
                            {"context": "Sprint 1 walking skeleton (end-to-end)"},
                        ]
                    },
                },
            ],
        },
    )
    notes = check_governance(fake_get, "acme/clarion", "main")
    assert any("active ruleset 'main release gate'" in note for note in notes)
    assert any("active tag ruleset(s) protect refs/tags/v*" in note for note in notes)

    # Tag protection absent (GOV-02): a fully-protected branch but no tag
    # ruleset must still fail — a tag-push could point a release tag at an
    # arbitrary commit.
    responses["/repos/acme/clarion/rulesets"] = ApiResponse(
        200,
        [
            {
                "id": 43,
                "name": "main release gate",
                "target": "branch",
                "enforcement": "active",
                "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            }
        ],
    )
    try:
        check_governance(fake_get, "acme/clarion", "main")
    except CheckError as exc:
        assert "no active repository ruleset protects refs/tags/v*" in str(exc)
    else:
        raise AssertionError("missing tag ruleset fixture should fail")

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        workflow = root / ".github" / "workflows" / "ci.yml"
        workflow.parent.mkdir(parents=True)
        workflow.write_text(
            "jobs:\n"
            "  ok:\n"
            "    steps:\n"
            "      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5\n",
            encoding="utf-8",
        )
        dependabot = root / ".github" / "dependabot.yml"
        dependabot.write_text(
            'version: 2\n'
            'updates:\n'
            '  - package-ecosystem: "github-actions"\n'
            '    directory: "/"\n'
            '    schedule:\n'
            '      interval: "weekly"\n',
            encoding="utf-8",
        )
        assert check_workflow_action_pins(root)
        assert check_dependabot_github_actions_updates(root)
        workflow.write_text(
            "jobs:\n"
            "  bad:\n"
            "    steps:\n"
            "      - uses: actions/checkout@v4\n",
            encoding="utf-8",
        )
        try:
            check_workflow_action_pins(root)
        except CheckError as exc:
            assert "not a full commit SHA" in str(exc)
        else:
            raise AssertionError("tag-pinned fixture should fail")
        dependabot.write_text(
            'version: 2\n'
            'updates:\n'
            '  - package-ecosystem: "pip"\n'
            '    directory: "/plugins/python"\n',
            encoding="utf-8",
        )
        try:
            check_dependabot_github_actions_updates(root)
        except CheckError as exc:
            assert "missing package-ecosystem=github-actions" in str(exc)
        else:
            raise AssertionError("missing github-actions Dependabot fixture should fail")

        release_workflow = workflow.parent / "release.yml"
        release_workflow.write_text(
            "jobs:\n"
            "  verify:\n"
            "    steps: []\n"
            "  release-governance:\n"
            "    steps: []\n"
            "  build-rust:\n"
            "    needs: [verify]\n"
            "    steps: []\n"
            "  build-plugin:\n"
            "    needs: [verify]\n"
            "    steps: []\n",
            encoding="utf-8",
        )
        try:
            check_release_workflow_governance_gate(root)
        except CheckError as exc:
            assert "build-rust" in str(exc)
            assert "release-governance" in str(exc)
            assert "must need verify" in str(exc)
        else:
            raise AssertionError("ungated release-build fixture should fail")

        release_workflow.write_text(
            "jobs:\n"
            "  verify:\n"
            "    steps: []\n"
            "  release-governance:\n"
            "    needs: [verify]\n"
            "    steps: []\n"
            "  build-rust:\n"
            "    needs: [verify, release-governance]\n"
            "    steps: []\n"
            "  build-plugin:\n"
            "    needs: [verify, release-governance]\n"
            "    steps: []\n",
            encoding="utf-8",
        )
        assert check_release_workflow_governance_gate(root)

    repo_root = Path(__file__).resolve().parents[1]
    assert check_governance_doc_lockstep(repo_root)

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
        "--repo-root",
        type=Path,
        default=Path.cwd(),
        help="repository root for static workflow-pin checks",
    )
    parser.add_argument(
        "--api-base",
        default=os.environ.get("GITHUB_API_URL", "https://api.github.com"),
        help="GitHub API base URL",
    )
    parser.add_argument("--self-test", action="store_true", help="run built-in tests")
    parser.add_argument(
        "--static-only",
        action="store_true",
        help="run source-tree checks and skip live GitHub API checks",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        run_self_test()
        return 0

    try:
        static_notes = [
            *check_workflow_action_pins(args.repo_root),
            *check_dependabot_github_actions_updates(args.repo_root),
            *check_release_workflow_governance_gate(args.repo_root),
            *check_governance_doc_lockstep(args.repo_root),
        ]
    except CheckError as exc:
        print("GitHub release governance guard failed:", file=sys.stderr)
        print(str(exc), file=sys.stderr)
        return 1
    except UsageError as exc:
        print(f"GitHub release governance guard could not run: {exc}", file=sys.stderr)
        return 2

    if args.static_only:
        for note in static_notes:
            print(f"ok: {note}")
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
        notes = [*static_notes, *check_governance(client.get, args.repository, args.branch)]
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
