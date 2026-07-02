from typing import Optional, List, Dict, Tuple, Any

# Type hint for the `nc_to_columns` function
def nc_to_columns(
    input: str,
    initial_state: Optional[str] = None,
    axis_identifiers: Optional[List[str]] = None,
    extra_axes: Optional[List[str]] = None,
    iteration_limit: int = 10000,
    disable_forward_fill: bool = False,
    axis_index_map: Optional[Dict[str, int]] = None,
    allow_undefined_variables: bool = False,
) -> Tuple[Dict[str, List[Any]], List[Tuple[str, str]], Dict[str, Dict[str, float]]]:
    """
    Interpret an NC program and return plain columnar data.

    Returns:
    --------
    Tuple[Dict[str, List[Any]], List[Tuple[str, str]], Dict[str, Dict[str, float]]]
        A tuple of:
        - data: mapping of column name to a list of cell values (None = null)
        - schema: ordered list of (column name, dtype name) pairs, where the
          dtype name is one of "f64", "i64", "str", "list[str]"
        - state: nested dictionary representing the interpreter state after
          execution

    The Python wrapper (`nc_gcode_interpreter.nc_to_dataframe`) assembles a
    polars DataFrame from this; the Rust extension itself has no polars
    dependency.
    """
    ...

__all__ = ["nc_to_columns"]
