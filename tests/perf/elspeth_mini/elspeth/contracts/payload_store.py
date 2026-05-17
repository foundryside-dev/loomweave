"""PayloadStore protocol for content-addressable blob storage.

This protocol defines the interface for payload storage backends used by:
- core/payload_store.py (FilesystemPayloadStore implementation)
- core/retention/purge.py (PurgeManager for retention policy)

Consolidated here to avoid circular imports and provide single source of truth.

IntegrityError and PayloadNotFoundError are the complete exception vocabulary
for this protocol — one for corruption, one for absence. Do not add further
exception subtypes without strong justification.
"""

from typing import Protocol, runtime_checkable


class IntegrityError(Exception):
    """Raised when payload content doesn't match expected hash.

    This indicates either filesystem corruption, tampering, or a bug.
    For an audit system, this is a critical failure that must be
    investigated - we never silently return corrupted data.
    """

    pass


class PayloadNotFoundError(Exception):
    """Raised when a payload blob is not found (purged, stale reference).

    This is a normal operational condition — retention policies purge old
    payloads. Callers decide whether to degrade gracefully or propagate.
    """

    def __init__(self, content_hash: str) -> None:
        if not content_hash:
            raise ValueError("PayloadNotFoundError requires a non-empty content_hash")
        self.content_hash = content_hash
        super().__init__(f"Payload not found: {content_hash}")


@runtime_checkable
class PayloadStore(Protocol):
    """Protocol for payload storage backends.

    All implementations must provide content-addressable storage
    where payloads are stored by their SHA-256 hash.
    """

    def store(self, content: bytes) -> str:
        """Store content and return its hash.

        Args:
            content: Raw bytes to store

        Returns:
            SHA-256 hex digest of content
        """
        ...

    def retrieve(self, content_hash: str) -> bytes:
        """Retrieve content by hash with integrity verification.

        Args:
            content_hash: SHA-256 hex digest

        Returns:
            Original content bytes

        Raises:
            PayloadNotFoundError: If content not found
            IntegrityError: If content doesn't match expected hash
        """
        ...

    def exists(self, content_hash: str) -> bool:
        """Check if content exists.

        Args:
            content_hash: SHA-256 hex digest

        Returns:
            True if content exists
        """
        ...

    def delete(self, content_hash: str) -> bool:
        """Delete content by hash.

        Args:
            content_hash: SHA-256 hex digest

        Returns:
            True if content was deleted, False if not found
        """
        ...
