from collections.abc import Iterator
from typing import Any

# Type stubs for the compiled Rust extension `nc_gcode_interpreter._internal`.
# mypy cannot introspect the pyo3 module, so the public entry points the Python
# wrapper imports are declared here. Signatures mirror the `#[pyo3(signature)]`
# defaults in src/lib.rs.

class NcError(ValueError):
    """NC parse/interpret error carrying structured location data. Subclasses
    ValueError so `except ValueError` keeps working."""

    #: Stable, machine-readable discriminator for the error class (e.g.
    #: ``"unexpected_axis"``, ``"undefined_variable"``), for branching without
    #: matching the formatted message.
    kind: str
    line: int | None
    column: int | None
    context: str | None
    line_text: str | None

def nc_to_rows(
    input: str,
    initial_state: str | None = None,
    axis_identifiers: list[str] | None = None,
    extra_axes: list[str] | None = None,
    iteration_limit: int = 10000,
    forward_fill: bool = True,
    include_variables: bool = False,
    axis_index_map: dict[str, int] | None = None,
    allow_undefined_variables: bool = False,
    input_is_path: bool = False,
    flatten_tolerance: float | None = None,
) -> Iterator[tuple[Any, ...]]:
    """Interpret an NC program lazily into ``(line_no, row[, variables])`` tuples."""

def nc_to_batches(
    input: str,
    batch_size: int = 500_000,
    initial_state: str | None = None,
    axis_identifiers: list[str] | None = None,
    extra_axes: list[str] | None = None,
    iteration_limit: int = 10000,
    disable_forward_fill: bool = False,
    axis_index_map: dict[str, int] | None = None,
    allow_undefined_variables: bool = False,
    input_is_path: bool = False,
    flatten_tolerance: float | None = None,
    include_line_numbers: bool = False,
    include_variables: bool = False,
) -> Any:
    """Interpret an NC program into an iterator of columnar polars DataFrames.

    The returned iterator exposes ``state`` (dict with ``axes``,
    ``symbol_table``, ``translation`` and ``string_table``) once exhausted, and
    - when ``include_variables`` is set - ``variable_events`` (an Arrow batch of
    ``row_idx`` / ``name_id`` / ``value``) and ``variable_names`` (list[str]).
    """

__all__ = ["nc_to_batches", "nc_to_rows"]
