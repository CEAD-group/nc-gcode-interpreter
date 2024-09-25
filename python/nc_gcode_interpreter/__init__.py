from .nc_gcode_interpreter import nc_to_dataframe as _nc_to_dataframe
from .nc_gcode_interpreter import __doc__  # noqa: F401

__all__ = ["nc_to_dataframe"]


# nc_gcode_interpreter.pyi
import polars as pl
from typing import Protocol


class TextFileLike(Protocol):
    def read(self) -> str: ...


def nc_to_dataframe(
    input: TextFileLike | str,
    initial_state: TextFileLike | str | None = None,
    axis_identifiers: list[str] | None = None,
    extra_axes: list[str] | None = None,
    iteration_limit: int = 10000,
) -> tuple[pl.DataFrame, dict]:
    if input is None:
        raise ValueError("input cannot be None")
    if not isinstance(input, str):
        input = input.read()
    if initial_state is not None and not isinstance(initial_state, str):
        initial_state = initial_state.read()

    df, state = _nc_to_dataframe(
        input, initial_state, axis_identifiers, extra_axes, iteration_limit
    )
    return df, state
