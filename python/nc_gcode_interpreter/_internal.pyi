from typing import Any, Iterator, Optional, List, Dict, Tuple

# Type stubs for the compiled Rust extension `nc_gcode_interpreter._internal`.
# mypy cannot introspect the pyo3 module, so the public entry points the Python
# wrapper imports are declared here. Signatures mirror the `#[pyo3(signature)]`
# defaults in src/lib.rs.

class NcError(ValueError):
    """NC parse/interpret error carrying structured location data. Subclasses
    ValueError so `except ValueError` keeps working."""

    line: Optional[int]
    column: Optional[int]
    context: Optional[str]
    line_text: Optional[str]

def nc_to_rows(
    input: str,
    initial_state: Optional[str] = None,
    axis_identifiers: Optional[List[str]] = None,
    extra_axes: Optional[List[str]] = None,
    iteration_limit: int = 10000,
    forward_fill: bool = True,
    include_variables: bool = False,
    axis_index_map: Optional[Dict[str, int]] = None,
    allow_undefined_variables: bool = False,
    input_is_path: bool = False,
    flatten_tolerance: Optional[float] = None,
) -> Iterator[Tuple[Any, ...]]:
    """Interpret an NC program lazily into ``(line_no, row[, variables])`` tuples."""
    ...

def nc_to_batches(
    input: str,
    batch_size: int = 500_000,
    initial_state: Optional[str] = None,
    axis_identifiers: Optional[List[str]] = None,
    extra_axes: Optional[List[str]] = None,
    iteration_limit: int = 10000,
    disable_forward_fill: bool = False,
    axis_index_map: Optional[Dict[str, int]] = None,
    allow_undefined_variables: bool = False,
    input_is_path: bool = False,
    flatten_tolerance: Optional[float] = None,
    include_line_numbers: bool = False,
) -> Any:
    """Interpret an NC program into an iterator of columnar polars DataFrames."""
    ...

__all__ = ["nc_to_rows", "nc_to_batches"]
