from typing import Protocol
import polars as pl
from ._internal import nc_to_dataframe as _nc_to_dataframe
from ._internal import __doc__  # noqa: F401


# Define TextFileLike Protocol
class TextFileLike(Protocol):
    def read(self) -> str: ...


__all__ = ["nc_to_dataframe"]


def nc_to_dataframe(
    input: "TextFileLike | str",
    initial_state: "TextFileLike | str | None" = None,
    axis_identifiers: "list[str] | None" = None,
    extra_axes: "list[str] | None" = None,
    iteration_limit: int = 10000,
    disable_forward_fill: bool = False,
) -> tuple[pl.DataFrame, dict]:
    """
    Convert G-code to a DataFrame representation along with the state information.

    Parameters:
    -----------
    input: TextFileLike | str
        The G-code input as a string or a file-like object.
    initial_state: TextFileLike | str | None
        An optional initial state string or a file-like object.
    axis_identifiers: list[str] | None
        A list of axis identifiers.
    extra_axes: list[str] | None
        A list of extra axes to be included.
    iteration_limit: int
        The maximum number of iterations to process.
    disable_forward_fill: bool
        Whether to disable forward-filling of values.

    Returns:
    --------
    tuple[pl.DataFrame, dict]
        A tuple containing the resulting DataFrame and a nested dictionary representing the state.
    """
    if input is None:
        raise ValueError("input cannot be None")
    if not isinstance(input, str):
        input = input.read()
    if initial_state is not None and not isinstance(initial_state, str):
        initial_state = initial_state.read()

    df, state = _nc_to_dataframe(
        input,
        initial_state,
        axis_identifiers,
        extra_axes,
        iteration_limit,
        disable_forward_fill,
    )
    return df, state
