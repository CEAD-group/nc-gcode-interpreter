"""NcError exposes an error's source location as data (line / column / context
/ line_text) instead of only as a formatted string, while remaining a
ValueError subclass so existing `except ValueError` handlers keep working."""

import pytest
from nc_gcode_interpreter import NcError, nc_to_dataframe


def test_nc_error_is_a_value_error_subclass():
    assert issubclass(NcError, ValueError)
    with pytest.raises(ValueError):
        nc_to_dataframe("X=R99\n")  # undefined variable


def test_syntax_error_carries_line_and_column():
    # The malformed token is on line 2; the parser reports its column.
    with pytest.raises(NcError) as exc:
        nc_to_dataframe("G1 X10\nX20 Y((\nX30\n")
    e = exc.value
    assert e.line == 2
    assert isinstance(e.column, int) and e.column > 0
    assert e.line_text == "X20 Y(("
    assert e.context == "line parsing"


def test_semantic_error_carries_line_but_no_column():
    # An undefined-variable error is anchored to a line, not a column.
    with pytest.raises(NcError) as exc:
        nc_to_dataframe("X=R99\n")
    e = exc.value
    assert e.line == 1
    assert e.column is None
    assert e.line_text == "X=R99"


def test_location_attributes_always_present():
    # Every NcError has the four attributes, even when a value is None, so
    # callers can read them unconditionally.
    with pytest.raises(NcError) as exc:
        nc_to_dataframe("G999 X1\n")  # unknown G code
    e = exc.value
    assert e.line == 1
    for attr in ("line", "column", "context", "line_text"):
        assert hasattr(e, attr)


def test_str_is_still_the_full_message():
    with pytest.raises(NcError) as exc:
        nc_to_dataframe("X=R99\n")
    assert "Undefined variable" in str(exc.value)
