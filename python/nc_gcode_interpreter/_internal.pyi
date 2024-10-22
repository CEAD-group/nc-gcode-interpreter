from typing import Optional, List, Dict, Tuple
import polars as pl

# Define the type hint for the `nc_to_dataframe` function
def nc_to_dataframe(
    input: str,
    initial_state: Optional[str] = None,
    axis_identifiers: Optional[List[str]] = None,
    extra_axes: Optional[List[str]] = None,
    iteration_limit: int = 10000,
    disable_forward_fill: bool = False,
) -> Tuple[pl.DataFrame, Dict[str, Dict[str, float]]]:
    """
    Convert G-code to a DataFrame representation along with the state information.

    Parameters:
    -----------
    input: str
        The G-code input as a string.
    initial_state: Optional[str]
        An optional initial state string.
    axis_identifiers: Optional[List[str]]
        A list of axis identifiers.
    extra_axes: Optional[List[str]]
        A list of extra axes to be included.
    iteration_limit: int
        The maximum number of iterations to process.
    disable_forward_fill: bool
        Whether to disable forward-filling of values.

    Returns:
    --------
    Tuple[pl.DataFrame, Dict[str, Dict[str, float]]]
        A tuple containing the resulting DataFrame and a nested dictionary representing the state.
    """
    ...

# Module definition for nc_gcode_interpreter
# Since this is an auto-generated module, the binding name corresponds to the compiled Rust module.

__all__ = ["nc_to_dataframe"]
