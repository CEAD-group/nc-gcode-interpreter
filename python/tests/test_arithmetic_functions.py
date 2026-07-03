"""Arithmetic functions from NC programming manual 4.1.3.1 / 4.1.1.13."""

import pytest
from nc_gcode_interpreter import nc_to_dataframe


@pytest.mark.parametrize(
    "expression, expected",
    [
        ("ROUNDUP(3.1)", 4.0),
        ("ROUNDUP(3.0)", 3.0),
        ("ROUNDUP(-3.1)", -3.0),
        ("MINVAL(3, -2)", -2.0),
        ("MAXVAL(3, -2)", 3.0),
        ("BOUND(-5, 5, 7.2)", 5.0),
        ("BOUND(-5, 5, -7.2)", -5.0),
        ("BOUND(-5, 5, 3)", 3.0),
    ],
)
def test_arithmetic_function_values(expression, expected):
    df, _state = nc_to_dataframe(f"X={expression}")
    assert df["X"][0] == expected


def test_bound_with_inverted_limits_raises():
    with pytest.raises(ValueError, match="minimum"):
        nc_to_dataframe("X=BOUND(5, -5, 1)")


def test_reserved_word_prefix_with_underscore_is_an_identifier():
    """TO_X starts with the reserved word TO; the word boundary must include
    the underscore so it still parses as an ordinary variable name."""
    df, state = nc_to_dataframe("DEF REAL TO_X = 1.5\nX=TO_X")
    assert state["symbol_table"]["TO_X"] == 1.5
    assert df["X"].to_list() == [1.5]
