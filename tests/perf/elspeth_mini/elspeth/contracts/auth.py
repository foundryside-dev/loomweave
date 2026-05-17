"""Authentication contracts shared across web identity boundaries.

Layer: L0 (contracts). No upward imports.
"""

from __future__ import annotations

from typing import Literal

AuthProviderType = Literal["local", "oidc", "entra"]
"""Closed discriminator for configured web authentication providers."""
