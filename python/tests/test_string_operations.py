"""String operations from NC programming manual 4.1.4: single-character
writes, `<<` concatenation, and the SPRINT/SUBSTR/INDEX/NUMBER family."""

import pytest
from nc_gcode_interpreter import nc_to_dataframe


def test_single_character_write_replaces_one_character():
    """STRING[<index>] = "<char>" overwrites one character in place (manual
    4.1.4.8); index 5 is the space in "AXIS N HERE" (0-based)."""
    _df, state = nc_to_dataframe('DEF STRING[13] MSG = "AXIS N HERE"\nMSG[5] = "X"\n')
    assert state["string_table"]["MSG"] == "AXIS X HERE"


def test_date_formatting_idiom_zero_pads_with_index_and_char_write():
    """The real CAM idiom that motivated this feature: SPRINT a date (its
    `%2d` fields space-pad single digits), then replace every space with "0"
    via a WHILE/INDEX/char-write loop."""
    program = (
        "DEF STRING[13] DT\n"
        'DT = SPRINT("20%2d%2d%2dT%2d%2d", 24, 5, 29, 13, 31)\n'
        'WHILE (INDEX(DT, " ") > 0)\n'
        '  DT[INDEX(DT, " ")] = "0"\n'
        "ENDWHILE\n"
    )
    _df, state = nc_to_dataframe(program)
    assert state["string_table"]["DT"] == "20240529T1331"


def test_concat_operator_joins_strings_and_formats_numbers():
    """`<<` joins quoted strings and STRING variables; an INT converts to
    plain form and a REAL keeps up to 10 decimals with trailing zeros trimmed
    (manual 4.1.4.1) - distinct from SPRINT `%F`'s fixed six decimals."""
    program = (
        "DEF STRING[13] DT = \"20240529T1331\"\n"
        "DEF STRING[100] WF\n"
        'WF = "//NC:/DIR/CAL_" << DT << ".TXT"\n'
        "DEF INT IDX = 2\n"
        "DEF REAL VAL = 9.654\n"
        "DEF STRING[50] MSGS\n"
        'MSGS = "i:" << IDX << "/v:" << VAL\n'
    )
    _df, state = nc_to_dataframe(program)
    assert state["string_table"]["WF"] == "//NC:/DIR/CAL_20240529T1331.TXT"
    assert state["string_table"]["MSGS"] == "i:2/v:9.654"


@pytest.mark.parametrize(
    "expression, expected",
    [
        ('NUMBER(SUBSTR("20240529T1331", 2, 2))', 24.0),
        ('STRLEN("20240529T1331")', 13.0),
        ('INDEX("20240529T1331", "T")', 8.0),
        ('INDEX("20240529T1331", "Q")', -1.0),
        ('RINDEX("20240529T1331", "3")', 11.0),
        ('ISNUMBER("12.5")', 1.0),
        ('ISNUMBER("x9")', 0.0),
    ],
)
def test_string_query_functions(expression, expected):
    """INDEX/RINDEX/STRLEN/ISNUMBER are 0-based; the search family returns -1
    when the character is not found (manual 4.1.4.6)."""
    _df, state = nc_to_dataframe(f"R1={expression}")
    assert state["symbol_table"]["R1"] == expected


def test_number_on_non_numeric_string_raises():
    """NUMBER on a string that is not a valid number is a loud failure, never
    a silent 0 (manual 4.1.4.2)."""
    with pytest.raises(ValueError, match="not a valid number"):
        nc_to_dataframe('R1 = NUMBER("abc")')


def test_whitespace_only_string_literal_keeps_its_content():
    """Regression: a quoted string that is only whitespace (e.g. " ") must
    keep its content rather than being eaten by implicit whitespace skipping -
    exactly the space the date-zeroing idiom above searches for."""
    _df, state = nc_to_dataframe('DEF STRING[8] STR = " X"\nR1 = INDEX(STR, " ")\n')
    assert state["symbol_table"]["R1"] == 0.0
