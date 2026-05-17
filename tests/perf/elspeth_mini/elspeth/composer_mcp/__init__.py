"""MCP server for ELSPETH pipeline composition.

Exposes the same pipeline-building tools as the web composer,
without Landscape access. Uses CatalogService for plugin discovery
and CompositionState for state mutation/validation.
"""


def create_server(*args, **kwargs):  # type: ignore[no-untyped-def]
    """Create a composer MCP server instance. Requires the [mcp] extra."""
    from elspeth.composer_mcp.server import create_server as _create_server

    return _create_server(*args, **kwargs)


def main(*args, **kwargs):  # type: ignore[no-untyped-def]
    """Run the composer MCP server. Requires the [mcp] extra."""
    from elspeth.composer_mcp.server import main as _main

    return _main(*args, **kwargs)


__all__ = ["create_server", "main"]
