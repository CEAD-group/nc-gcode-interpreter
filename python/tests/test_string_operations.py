"""String operations from NC programming manual 4.1.4: single-character
writes, `<<` concatenation, and the SPRINT/SUBSTR/INDEX/NUMBER family."""

import pytest
from nc_gcode_interpreter import NcError, nc_to_dataframe
from nc_source import nc_lines


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


def test_match_and_mindex_search_for_strings_and_character_lists():
    """MATCH returns the 0-based position of a substring and MINDEX that of
    the first character found in a character list, -1 when absent (manual
    4.1.4.6; the MINDEX values are the manual's own example)."""
    program = nc_lines(
        'DEF STRING[30] INP = "/_N_MPF_DIR/_N_EXECUTE_MPF"',
        'R1 = MATCH(INP, "EXECUTE")',
        'R2 = MATCH(INP, "NOPE")',
        'R3 = MINDEX(INP, "M,N,O,P")',
        'R4 = MINDEX(INP, "QWZ")',
    )
    _df, state = nc_to_dataframe(program)
    assert state["symbol_table"]["R1"] == 15.0
    assert state["symbol_table"]["R2"] == -1.0
    assert state["symbol_table"]["R3"] == 2.0
    assert state["symbol_table"]["R4"] == -1.0


def test_concat_prefix_form_is_accepted_in_function_arguments():
    """`<<` with no left operand is the implicit conversion to STRING (manual
    4.1.4.1). Real programs compare machine data against an expected name with
    `MATCH(<string>, <<NAME)` inside an IF condition (#64)."""
    program = nc_lines(
        'DEF STRING[16] EXPECTED = "CELL-7"',
        'DEF STRING[32] ID_INFO = "ROBOT CELL-7 V2"',
        "DEF INT RES = 0",
        "IF ((MATCH(ID_INFO, <<EXPECTED)) == -1)",
        "  RES = 1",
        "ELSE",
        "  RES = 2",
        "ENDIF",
        "R1 = MATCH(ID_INFO, <<EXPECTED)",
        'R2 = STRLEN("A" << "BC")',
    )
    _df, state = nc_to_dataframe(program)
    assert state["symbol_table"]["RES"] == 2.0
    assert state["symbol_table"]["R1"] == 6.0
    assert state["symbol_table"]["R2"] == 3.0


def test_deeply_nested_function_calls_parse_in_linear_time():
    """Regression (#78): each function argument is parsed once. Trying a `<<`
    chain before a plain expression re-parsed every nested argument (4^n), so
    a 24-level call would not finish; it must be instant."""
    program = "R1 = " + "SIN(" * 24 + "0" + ")" * 24 + "\n"
    _df, state = nc_to_dataframe(program)
    assert state["symbol_table"]["R1"] == 0.0


def test_unknown_system_variable_in_string_argument_is_loud():
    """There is no machine-data model: reading an unknown `$`-variable where a
    string is expected names the variable and the line, never a silent empty
    string. allow_undefined_variables keeps its documented meaning (the read
    yields 0, so MATCH finds nothing)."""
    program = nc_lines(
        'DEF STRING[16] EXPECTED = "CELL-7"',
        "R1 = MATCH($MN_AXCONF_MACHAX_NAME_TAB[0], <<EXPECTED)",
    )
    with pytest.raises(NcError) as exc:
        nc_to_dataframe(program)
    assert exc.value.line == 2
    assert "$MN_AXCONF_MACHAX_NAME_TAB[0]" in str(exc.value)
    _df, state = nc_to_dataframe(program, allow_undefined_variables=True)
    assert state["symbol_table"]["R1"] == -1.0


def test_date_zero_padding_with_system_variable_timestamp():
    """The calibration-date idiom from #58: SPRINT a timestamp from the $A_*
    clock variables, then zero-pad with a WHILE/INDEX/char-write loop. The
    clock values come from the initial state."""
    program = nc_lines(
        "DEF STRING[13] CAL_DATE",
        'CAL_DATE = SPRINT("20%2d%2d%2dT%2d%2d", $A_YEAR,$A_MONTH,$A_DAY,$A_HOUR,$A_MINUTE)',
        'WHILE (INDEX(CAL_DATE, " ") > 0)',
        '  CAL_DATE[INDEX(CAL_DATE, " ")] = "0"',
        "ENDWHILE",
    )
    clock = nc_lines("$A_YEAR=24", "$A_MONTH=5", "$A_DAY=9", "$A_HOUR=7", "$A_MINUTE=3")
    _df, state = nc_to_dataframe(program, initial_state=clock)
    assert state["string_table"]["CAL_DATE"] == "20240509T0703"


def test_string_argument_to_numeric_function_is_a_type_error():
    """A quoted string handed to a numeric function is a type error naming the
    string, not a silently dropped argument."""
    with pytest.raises(ValueError, match="expected a number"):
        nc_to_dataframe('R1 = SIN("abc")')
