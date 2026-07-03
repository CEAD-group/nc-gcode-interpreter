"""Diagnostics: silent-misparse guards, human parse errors, did-you-mean."""

import pytest
from nc_gcode_interpreter import nc_to_dataframe


def test_unknown_g_code_errors():
    """A bare G<digits> word that matches no G group is an unknown G code
    (alarm 12470 on a real control), not a silent subprogram call."""
    with pytest.raises(ValueError, match="not a G code"):
        nc_to_dataframe("G1 X10\nG999\nX5")


def test_lowercase_unknown_g_code_errors():
    with pytest.raises(ValueError, match="not a G code"):
        nc_to_dataframe("g999")


def test_axis_word_typo_warns_but_keeps_call_semantics(capfd):
    """Y2O (letter O typo for Y20) stays a subprogram call, but the silent
    loss of the intended axis move is flagged on stderr."""
    df, _state = nc_to_dataframe("G1 X10 Y2O\nX5")
    assert df["non_returning_function_call"][0] == "Y2O"
    assert "Y2O" in capfd.readouterr().err


def test_call_with_arguments_does_not_warn(capfd):
    df, _state = nc_to_dataframe("SETAL(67037)\nX5")
    assert "Warning" not in capfd.readouterr().err


def test_parse_errors_use_human_phrasing():
    """Grammar-internal rule names (label_name, frame_kw, ...) must not
    leak into parse error messages."""
    with pytest.raises(ValueError) as excinfo:
        nc_to_dataframe("G1 X10 Y=")
    message = str(excinfo.value)
    assert "label_name" not in message
    assert "frame_kw" not in message
    assert "axis_increment" not in message
    assert "expected" in message


@pytest.mark.parametrize(
    "program, expected_line, expected_message",
    [
        ("X1\nIF R1>0\nX2\nX3", 2, "IF is never closed"),
        ("WHILE R1<5\nX1", 1, "WHILE is never closed"),
        ("X1\nENDIF", 2, "ENDIF without a preceding IF"),
        ("X1\nELSE\nX2", 2, "ELSE without a preceding IF"),
        ("WHILE R1<5\nX1\nENDIF", 3, "innermost open structure is WHILE from line 1"),
        ("IF R1>0\nWHILE R2<5\nX1\nENDIF\nENDWHILE", 4, "innermost open structure is WHILE from line 2"),
        # Nested unclosed structures: the innermost one is the actionable fix
        # (its closer must come first).
        ("IF R1>0\nWHILE R2<5\nX1", 2, "WHILE is never closed"),
        # The grammar allows whitespace between N and the block number.
        ("N 10 IF R1>0\nX1", 1, "IF is never closed"),
    ],
    ids=[
        "missing-ENDIF",
        "missing-ENDWHILE",
        "stray-ENDIF",
        "stray-ELSE",
        "wrong-closer",
        "crossed-nesting",
        "nested-unclosed-reports-innermost",
        "block-number-with-space",
    ],
)
def test_unmatched_structures_point_at_the_cause(program, expected_line, expected_message):
    """Unclosed or crossed control structures must name the offending line,
    not fail at the end of the file like a raw PEG parse does."""
    with pytest.raises(ValueError) as excinfo:
        nc_to_dataframe(program)
    message = str(excinfo.value)
    assert expected_message in message
    assert f"line {expected_line}" in message


def test_single_line_conditional_jump_is_not_an_opener():
    df, _state = nc_to_dataframe("R1=1\nIF R1>0 GOTOF DONE\nX999\nDONE: X1")
    assert df["X"].drop_nulls().to_list() == [1.0]


def test_label_starting_with_structure_keyword_is_not_an_opener():
    """A label like REPEAT_HERE: (or even UNTIL_X:) must not be taken for a
    structure keyword by the pre-scan."""
    df, _state = nc_to_dataframe("GOTOF UNTIL_X\nX999\nUNTIL_X: X1")
    assert df["X"].to_list() == [1.0]


def test_jump_error_suggests_similar_label():
    with pytest.raises(ValueError, match="Did you mean 'ENDE'"):
        nc_to_dataframe("GOTOF ENDF\nX1\nENDE: M30")


def test_jump_error_without_similar_label_has_no_suggestion():
    with pytest.raises(ValueError) as excinfo:
        nc_to_dataframe("GOTOF COMPLETELY_DIFFERENT\nX1\nENDE: M30")
    assert "Did you mean" not in str(excinfo.value)


def test_jump_to_existing_label_in_wrong_direction_hints_direction():
    """A label that exists but lies behind a GOTOF must not produce a
    'did you mean' with the very same name; the problem is the direction."""
    with pytest.raises(ValueError) as excinfo:
        nc_to_dataframe("BEHIND: X1\nGOTOF BEHIND\nX2\nM30")
    message = str(excinfo.value)
    assert "Did you mean" not in message
    assert "'BEHIND' is defined" in message
    assert "GOTOB" in message
