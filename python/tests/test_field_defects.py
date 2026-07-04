"""Regression tests for grammar/identifier defects found in a field test:

1. Leading-underscore variable names (`_WITH_M0`). Sinumerik user variables
   may start with `_`; CAM post-processors emit such temporaries routinely.
2. Assignment to `$AC_*` system variables (`$AC_TIMER[1] = 0`). There is no
   dedicated system-variable model, so these are stored like ordinary
   variables keyed by their full name.
3. The `NOT` logical operator (`YNEG = NOT YNEG` in MOVE_GRID.mpf). Unary,
   binds like unary minus; NOT 0 -> 1, NOT nonzero -> 0.
"""

import os

import pytest
from nc_gcode_interpreter import nc_to_dataframe


# --- Defect 1: leading-underscore identifiers ------------------------------


def test_leading_underscore_variable_definition_and_use():
    df, state = nc_to_dataframe(
        "DEF INT _WITH_M0 = 0\nDEF REAL _DWELLTIME = 3.0\nX=_DWELLTIME"
    )
    assert state["symbol_table"]["_WITH_M0"] == 0.0
    assert state["symbol_table"]["_DWELLTIME"] == 3.0
    assert df["X"][-1] == 3.0


def test_leading_underscore_variable_in_if_condition():
    """The exact shape from MOVE_GRID.mpf: `IF _WITH_M0` guarding an M0."""
    df, _state = nc_to_dataframe(
        "DEF INT _WITH_M0 = 1\nIF _WITH_M0\nM0\nENDIF\nX1"
    )
    assert df["M"].drop_nulls().to_list() == [["M0"]]


def test_leading_underscore_multiple_vars_one_def_line():
    """A single DEF mixing plain and underscore-prefixed names (as emitted)."""
    _df, state = nc_to_dataframe(
        "DEF INT GRID_STEPS, X_MOVES=0, YNEG=0, _WITH_M0 = 0"
    )
    assert state["symbol_table"]["_WITH_M0"] == 0.0
    assert state["symbol_table"]["X_MOVES"] == 0.0


def test_leading_underscore_survives_stage1_fast_path():
    """A structure-free program with an underscore assignment must produce the
    same result on the stage-1 fast path and the whole-file parse."""
    program = "_WITH_M0=5\nX=_WITH_M0"
    previous = os.environ.get("NC_STAGE1")
    try:
        os.environ["NC_STAGE1"] = "0"
        _full_df, full_state = nc_to_dataframe(program)
        os.environ["NC_STAGE1"] = "1"
        fast_df, fast_state = nc_to_dataframe(program)
    finally:
        if previous is None:
            os.environ.pop("NC_STAGE1", None)
        else:
            os.environ["NC_STAGE1"] = previous
    assert full_state["symbol_table"] == fast_state["symbol_table"]
    assert fast_state["symbol_table"]["_WITH_M0"] == 5.0
    assert fast_df["X"][-1] == 5.0


# --- Defect 2: assignment to $AC_* system variables ------------------------


def test_assign_system_variable_array_element():
    """The exact shape from Double_bead_rectangle.MPF line 158."""
    _df, state = nc_to_dataframe("$AC_TIMER[1] = 0\nX1")
    assert state["symbol_table"]["$AC_TIMER[1]"] == 0.0


def test_assign_scalar_system_variable():
    _df, state = nc_to_dataframe("$AC_MARKER = 42\nX1")
    assert state["symbol_table"]["$AC_MARKER"] == 42.0


def test_read_back_system_variable():
    df, state = nc_to_dataframe("$AC_TIMER[1] = 7\nX=$AC_TIMER[1]")
    assert state["symbol_table"]["$AC_TIMER[1]"] == 7.0
    assert df["X"][-1] == 7.0


# --- Defect 3: NOT logical operator -----------------------------------------


@pytest.mark.parametrize(
    ("program", "expected"),
    [
        ("R1=NOT 0", 1.0),
        ("R1=NOT 5", 0.0),
        ("R1=NOT -1", 0.0),  # innermost prefix (neg) applies first
        ("R1=1 + NOT 0", 2.0),  # binds like unary minus, below +
        ("DEF INT YNEG=0\nR1=0\nYNEG = NOT YNEG\nR1=YNEG", 1.0),
    ],
)
def test_not_operator_values(program, expected):
    _df, state = nc_to_dataframe(program)
    assert state["symbol_table"]["R1"] == expected


def test_not_toggle_shape_from_move_grid():
    """The exact shape from MOVE_GRID.mpf line 57: toggling a direction flag."""
    _df, state = nc_to_dataframe(
        "DEF INT YNEG=0\nYNEG = NOT YNEG\nYNEG = NOT YNEG\nYNEG = NOT YNEG"
    )
    assert state["symbol_table"]["YNEG"] == 1.0


def test_not_in_if_condition():
    _df, state = nc_to_dataframe("DEF INT FLAG=0\nIF NOT FLAG\nR9=42\nENDIF")
    assert state["symbol_table"]["R9"] == 42.0


def test_not_prefixed_identifier_is_not_the_operator():
    """Word boundary: NOTCH is an ordinary identifier, not NOT ~ CH."""
    _df, state = nc_to_dataframe("DEF INT NOTCH=7\nR1=NOTCH")
    assert state["symbol_table"]["R1"] == 7.0
