"""Protocol for plugins whose config schema is a Pydantic discriminated union.

Plugins implementing this protocol expose their variant model classes to the
composer's knob-schema lowering. See
docs/superpowers/specs/2026-05-14-composer-one-knob-design.md section 5.
"""

from __future__ import annotations

from typing import Protocol, runtime_checkable

from pydantic import BaseModel


@runtime_checkable
class DiscriminatedPlugin(Protocol):
    """Plugin protocol contract for discriminated-union config models.

    Implementing classes return:
        (discriminator_field_name, {literal_value: variant_model_cls})

    The discriminator field name must match the field on each variant model that
    carries the variant identifier, commonly ``provider``, ``kind``, or
    ``type``. Variant models must be ``Annotated[Union[...],
    Field(discriminator=...)]`` forms; the pydantic-v2 ``Discriminator(...)``
    class form is not supported.
    """

    @classmethod
    def discriminated_variants(cls) -> tuple[str, dict[str, type[BaseModel]]]: ...
