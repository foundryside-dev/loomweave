"""Database URL format validation for fail-fast config checking.

Shared between core/config.py and web/config.py to prevent validation
pattern drift — both subsystems must reject malformed URLs identically.
"""

from __future__ import annotations


def validate_database_url_format(url: str) -> str:
    """Validate that a string is a parseable database URL.

    Uses SQLAlchemy's own URL parser for accurate validation — the same
    parser that will later consume the URL at connection time.

    Raises ValueError for blank, malformed, or driver-less URLs.
    Returns the URL unchanged if valid.
    """
    if not url.strip():
        raise ValueError("database URL must not be blank (omit the field to use the default)")
    from sqlalchemy.engine.url import make_url
    from sqlalchemy.exc import ArgumentError

    try:
        parsed = make_url(url)
        if not parsed.drivername:
            raise ValueError("database URL missing driver (e.g., 'sqlite', 'postgresql')")
    except ArgumentError as e:
        raise ValueError(f"invalid database URL format: {e}") from e
    return url
