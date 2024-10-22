from typing import Protocol
import polars as pl
from ._internal import nc_to_dataframe as _nc_to_dataframe
from ._internal import __doc__  # noqa: F401
import json
from pathlib import Path
from typing import TypedDict, TypeVar, Any, Type, Callable, Generic


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


_T = TypeVar("_T")


class _classproperty(Generic[_T]):
    def __init__(self, fget: Callable[[Any], _T]) -> None:
        self.fget = fget

    def __get__(self, instance: Any, owner: Type[Any]) -> _T:
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
        with open(json_file, "r") as file:
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
