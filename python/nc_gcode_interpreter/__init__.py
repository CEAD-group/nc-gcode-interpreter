from typing import Protocol
import polars as pl
from ._internal import nc_to_columns as _nc_to_columns
from ._internal import nc_to_rows as _nc_to_rows
from ._internal import __doc__  # noqa: F401
import json
from pathlib import Path
from typing import TypedDict, TypeVar, Any, Generic
from collections.abc import Callable


# Define TextFileLike Protocol
class TextFileLike(Protocol):
    def read(self) -> str: ...


__all__ = ["nc_to_dataframe", "nc_to_rows", "sanitize_dataframe", "dataframe_to_nc"]


def nc_to_dataframe(
    input: TextFileLike | str,
    initial_state: TextFileLike | str | None = None,
    axis_identifiers: list[str] | None = None,
    extra_axes: list[str] | None = None,
    iteration_limit: int = 10000,
    disable_forward_fill: bool = False,
    axis_index_map: dict[str, int] | None = None,
    allow_undefined_variables: bool = False,
) -> tuple[pl.DataFrame, dict]:
    """
    Parses Sinumerik-flavored NC G-code and converts it into a Polars DataFrame along with the final state.

    This function processes the provided G-code input, interprets it according to Sinumerik specifications,
    and outputs the movement commands and other relevant data in a structured DataFrame format. It also
    returns a dictionary representing the final state after code execution, which can be useful for further analysis or inspection.

    Parameters:
    -----------
    input: TextFileLike | str
        The G-code input as a string or a file-like object.
    initial_state: TextFileLike | str | None, optional
        An optional initial state string or a file-like object to initialize the interpreter's state.
    axis_identifiers: list[str] | None, optional
        A list of axis identifiers to override the default axes (e.g., ['X', 'Y', 'Z']).
    extra_axes: list[str] | None, optional
        A list of extra axis identifiers to include in addition to the default ones (e.g., ['A', 'B', 'C']).
    iteration_limit: int, optional
        The maximum number of iterations to process, to prevent infinite loops in the G-code [default: 10000].
    disable_forward_fill: bool, optional
        If True, disables forward-filling of null values in axes columns in the resulting DataFrame.
    axis_index_map: dict[str, int] | None, optional
        A mapping from axis identifiers (e.g., 'E') to numeric indices (e.g., 4) for array assignments like FL[E]=10.
        This allows user-configurable mapping of axis names to indices. Example: {'E': 4, 'X': 0}.
    allow_undefined_variables: bool, optional
        If True, allows undefined variables to be used in expressions with a value of 0 [default: False].

    Returns:
    --------
    tuple[pl.DataFrame, dict]
        A tuple containing:
        - A Polars DataFrame representing the parsed G-code.
        - A nested dictionary representing the final state after execution.

    Raises:
    -------
    ValueError
        If the input is None or invalid.

    Example:
    --------
    >>> df, state = nc_to_dataframe('G1 X10 Y20 Z30')
    >>> print(df)
    shape: (1, 4)
    ┌─────────────┬──────┬──────┬──────┐
    │ gg01_motion ┆ X    ┆ Y    ┆ Z    │
    │ ---         ┆ ---  ┆ ---  ┆ ---  │
    │ str         ┆ f64  ┆ f64  ┆ f64  │
    ╞═════════════╪══════╪══════╪══════╡
    │ G1          ┆ 10.0 ┆ 20.0 ┆ 30.0 │
    └─────────────┴──────┴──────┴──────┘
    """
    if input is None:
        raise ValueError("input cannot be None")
    if not isinstance(input, str):
        input = input.read()
    if initial_state is not None and not isinstance(initial_state, str):
        initial_state = initial_state.read()

    data, schema, state = _nc_to_columns(
        input,
        initial_state,
        axis_identifiers,
        extra_axes,
        iteration_limit,
        disable_forward_fill,
        axis_index_map,
        allow_undefined_variables,
    )
    df = pl.DataFrame(
        data, schema={name: _DTYPES[dtype] for name, dtype in schema}
    )
    return df, state


_DTYPES: dict[str, pl.DataType | type[pl.DataType]] = {
    "f64": pl.Float64,
    "i64": pl.Int64,
    "str": pl.String,
    "list[str]": pl.List(pl.String),
}


_T = TypeVar("_T")


class _classproperty(Generic[_T]):
    def __init__(self, fget: Callable[[Any], _T]) -> None:
        self.fget = fget

    def __get__(self, instance: Any, owner: type[Any]) -> _T:
        return self.fget(owner)


class _GGroupEntry(TypedDict):
    id: str
    nr: int
    description: str


class _GGroup(TypedDict):
    nr: int
    title: str
    effectiveness: str
    short_name: str
    entries: list[_GGroupEntry]


class GGroups:
    _g_groups: list[_GGroup] | None = None
    _g_group_short_names: set[str] | None = None
    _g_groups_by_short_name: dict[str, _GGroup] | None = None

    @_classproperty
    def g_groups(cls) -> list[_GGroup]:
        if cls._g_groups is None:
            cls._load_data()
        assert cls._g_groups is not None
        return cls._g_groups

    @classmethod
    def _load_data(cls) -> None:
        json_file = Path(__file__).parent / "ggroups.json"
        with open(json_file) as file:
            g_groups = json.load(file)
            cls._g_groups = g_groups
            cls._g_group_short_names = {group["short_name"] for group in g_groups}
            cls._g_groups_by_short_name = {
                group["short_name"]: group for group in g_groups
            }

    @classmethod
    def is_g_group(cls, name: str) -> bool:
        if cls._g_group_short_names is None:
            cls._load_data()
        assert cls._g_group_short_names is not None
        return name in cls._g_group_short_names

    @classmethod
    def is_modal_g_group(cls, name: str) -> bool:
        if cls._g_groups_by_short_name is None:
            cls._load_data()
        assert cls._g_groups_by_short_name is not None
        return cls._g_groups_by_short_name[name]["effectiveness"] == "modal"


def sanitize_dataframe(
    df: pl.DataFrame, disable_forward_fill: bool = False
) -> pl.DataFrame:
    """
    Cleans and preprocesses the DataFrame resulting from G-code interpretation.

    This function performs necessary sanitization steps on the DataFrame, such as handling null values,
    forward-filling axis positions if enabled, and preparing the DataFrame for further processing or conversion.

    Parameters:
    -----------
    df: pl.DataFrame
        The DataFrame to sanitize.
    disable_forward_fill: bool, optional
        If True, disables forward-filling of null values in axes columns.

    Returns:
    --------
    pl.DataFrame
        The sanitized DataFrame ready for analysis or conversion back to G-code.
    """
    modal = [g["short_name"] for g in GGroups.g_groups if g["effectiveness"] == "modal"]
    non_modal = [g["short_name"] for g in GGroups.g_groups if g["effectiveness"] != "modal"]
    known_axes = [
        "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V",
        "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
    ]
    string_columns = {*modal, *non_modal, "T", "non_returning_function_call", "comment"}
    # Spline block addresses: value columns, but never forward-filled (a
    # point weight applies only to the point it is programmed with).
    block_addresses = ["PW", "SD", "PL"]

    # Value columns: anything that is not a known string/list column.
    value_columns = [
        c for c in df.columns if c not in string_columns and c not in ("M", "N")
    ]

    # Cast to the canonical dtypes.
    casts = []
    if "N" in df.columns:
        casts.append(pl.col("N").cast(pl.Int64))
    casts += [pl.col(c).cast(pl.String) for c in df.columns if c in string_columns]
    casts += [pl.col(c).cast(pl.Float64) for c in value_columns]
    if "M" in df.columns:
        casts.append(pl.col("M").cast(pl.List(pl.String)))
    df = df.with_columns(casts)

    # Canonical column order.
    order: list[str] = []
    for name in (
        ["N"] + modal + non_modal + known_axes
        + sorted(c for c in value_columns if c not in known_axes and c not in block_addresses)
        + block_addresses
        + ["T", "M", "non_returning_function_call", "comment"]
    ):
        if name in df.columns and name not in order:
            order.append(name)
    df = df.select(order)

    # Forward-fill value columns and modal G-group columns.
    if not disable_forward_fill:
        fill = [
            c
            for c in df.columns
            if (c in value_columns and c not in block_addresses) or c == "N" or c in modal
        ]
        df = df.with_columns([pl.col(c).fill_null(strategy="forward") for c in fill])
    return df


def dataframe_to_nc(df: pl.DataFrame, file_path: str | Path):
    """
    Converts a Polars DataFrame back into NC G-code and writes it to a file.

    This function takes a DataFrame representing G-code commands (as produced by `nc_to_dataframe`)
    and reconstructs the G-code, writing the output to the specified file path.

    Parameters:
    -----------
    df: pl.DataFrame
        The DataFrame containing G-code data to be converted back into NC code.
    file_path: str or Path
        The file path where the generated G-code should be written.

    Notes:
    ------
    - Currently, this function is implemented in Python. Future versions may implement this in Rust for performance.
    - The function ensures that consecutive duplicate values are appropriately handled to generate clean G-code.

    Example:
    --------
    >>> import polars as pl
    >>> from nc_gcode_interpreter import dataframe_to_nc
    >>> df = pl.DataFrame({'X': [10, 20], 'Y': [0, 0], 'Z': [0, 0]})
    >>> dataframe_to_nc(df, 'output.MPF')
    """
    df = sanitize_dataframe(df)
    # Python prototype of df to nc conversion code
    float_cols = [col for col in df.columns if df[col].dtype == pl.Float64]
    int_cols = [col for col in df.columns if df[col].dtype == pl.Int64]
    g_group_cols = [col for col in df.columns if GGroups.is_g_group(col)]
    list_of_str_cols = [
        col for col in df.columns if df[col].dtype == pl.List(pl.String)
    ]
    string_axes_cols = [col for col in df.columns if col in ["T"]]

    # Replace consecutive duplicates with null values
    df = df.with_columns(
        [
            pl.when(pl.col(c) == pl.col(c).shift(1))
            .then(None)
            .otherwise(
                pl.lit(f"{c}{'=' if len(c) > 1 else ''}")
                + pl.col(c).round(3).cast(pl.String)
            )
            .alias(c)
            for c in float_cols
        ]
        + [
            pl.when(pl.col(c) == pl.col(c).shift(1))
            .then(None)
            .otherwise(
                pl.lit(f"{c}{'=' if len(c) > 1 else ''}") + pl.col(c).cast(pl.String)
            )
            .alias(c)
            for c in int_cols
        ]
        + [
            (pl.lit(f'{c}="') + pl.col(c) + pl.lit('"')).alias(c)
            for c in string_axes_cols
        ]
        + [
            pl.when(pl.col(c) == pl.col(c).shift(1))
            .then(None)
            .otherwise(pl.col(c))
            .alias(c)
            for c in g_group_cols
        ]
        + [pl.col(c).list.join(separator=" ").alias(c) for c in list_of_str_cols]
    )

    # Define the columns you want to include in the output
    columns_of_interest = df.columns
    df_line = df.with_columns(
        pl.concat_str(
            [pl.col(c) for c in columns_of_interest], ignore_nulls=True, separator=" "
        ).alias("line")
    ).select("line")
    df_line.write_csv(file_path, include_header=False, quote_style="never")


def nc_to_rows(
    input: TextFileLike | str,
    initial_state: TextFileLike | str | None = None,
    axis_identifiers: list[str] | None = None,
    extra_axes: list[str] | None = None,
    iteration_limit: int = 10000,
    forward_fill: bool = True,
    include_variables: bool = False,
    axis_index_map: dict[str, int] | None = None,
    allow_undefined_variables: bool = False,
):
    """Interpret an NC program lazily, yielding one row at a time.

    Returns an iterator of ``(line_no, row)`` tuples: the 1-based source
    line the block came from (loops and jumps repeat / reorder line
    numbers), and a dict of column values typed like the batch DataFrame
    (``N``: int, ``M``: list[str], G-group/T/comment columns: str, axes and
    other value columns: float). Rows are forward-filled like
    :func:`nc_to_dataframe` unless ``forward_fill`` is False.

    With ``include_variables=True`` the iterator yields ``(line_no, row,
    variables)`` instead, where ``variables`` maps each variable the block
    assigned (``R1=R1+1``, ``DEF REAL Q=5``, FOR counters) to its new float
    value. Blocks that only assign variables — invisible in the batch
    DataFrame and in the default stream — are then also yielded, with an
    empty (never forward-filled) ``row``. Accumulating the ``variables``
    dicts with ``dict.update`` reconstructs the full variable state at any
    point of the stream.

    The interpreter runs on a background thread behind a bounded channel:
    rows arrive as they are produced, memory use is constant, and dropping
    the iterator aborts interpretation. Breaking out of a ``for`` loop over
    an anonymous iterator drops it; a stored iterator keeps the worker
    alive (parked on the bounded channel) until it is deleted or
    garbage-collected.
    Errors raise ``ValueError`` from ``next()`` when reached. After the
    iterator is exhausted, its ``state`` attribute holds the final
    interpreter state (axes, symbol_table, translation).

    Example:
    --------
    >>> for line_no, row in nc_to_rows("G1 X10\nX20 Y5"):
    ...     print(line_no, row["X"])
    1 10.0
    2 20.0
    >>> for line_no, row, variables in nc_to_rows("R1=5\nX=R1", include_variables=True):
    ...     print(line_no, row.get("X"), variables)
    1 None {'R1': 5.0}
    2 5.0 {}
    """
    if input is None:
        raise ValueError("input cannot be None")
    if not isinstance(input, str):
        input = input.read()
    if initial_state is not None and not isinstance(initial_state, str):
        initial_state = initial_state.read()
    return _nc_to_rows(
        input,
        initial_state,
        axis_identifiers,
        extra_axes,
        iteration_limit,
        forward_fill,
        include_variables,
        axis_index_map,
        allow_undefined_variables,
    )
