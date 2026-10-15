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
    _df, _state = nc_to_dataframe("SETAL(67037)\nX5")
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


@pytest.mark.parametrize(
    "program, expected_line, expected_message",
    [
        ("WHILE R1<3 X1\nR1=R1+1\nENDWHILE", 1, "unexpected text 'X1' on the WHILE line"),
        ("LOOP X1\nENDLOOP", 1, "unexpected text 'X1' on the LOOP line"),
        ("REPEAT X1\nUNTIL R1>=0", 1, "unexpected text 'X1' on the REPEAT line"),
        ("FOR R2=1 TO 3 X=R2\nENDFOR", 1, "unexpected text 'X=R2' on the FOR line"),
        ("IF R1==1\nX1\nELSE X2\nENDIF", 3, "unexpected text 'X2' on the ELSE line"),
        ("IF R1==1\nX1 ELSE X2\nENDIF", 2, "ELSE must start its own block"),
        ("IF R1==1\nX1 ELSE\nX2\nENDIF", 2, "ELSE must start its own block"),
        ("IF R1==0\nX1 ENDIF", 2, "ENDIF must start its own block"),
        ("WHILE R1<3\nR1=R1+1 ENDWHILE", 2, "ENDWHILE must start its own block"),
        ("X1 ELSE", 1, "ELSE must start its own block"),
        ("LB: WHILE R1<3\nR1=R1+1\nENDWHILE", 1, "jump label LB: is not allowed in a block with WHILE"),
        ("WHILE R1<3\nR1=R1+1\nLB: ENDWHILE", 3, "jump label LB: is not allowed in a block with ENDWHILE"),
        ("IF R1==1\nX1\nN10 LB: ELSE\nX2\nENDIF", 3, "jump label LB: is not allowed in a block with ELSE"),
    ],
    ids=[
        "statement-after-WHILE",
        "statement-after-LOOP",
        "statement-after-REPEAT",
        "statement-after-FOR",
        "statement-after-ELSE",
        "mid-line-ELSE",
        "trailing-ELSE",
        "mid-line-ENDIF",
        "mid-line-ENDWHILE",
        "mid-line-ELSE-without-structures",
        "label-on-WHILE",
        "label-on-ENDWHILE",
        "label-on-ELSE",
    ],
)
def test_statements_on_structure_lines_are_rejected(program, expected_line, expected_message):
    """A control-structure keyword stands alone in its block (manual 4.1.7),
    without a label (alarm 12630). These forms used to run silently, `WHILE
    R1<3 X1` with X1 as the first body block on every pass and `X1 ELSE X2`
    as an ELSE in mid-line (#84)."""
    with pytest.raises(ValueError) as excinfo:
        nc_to_dataframe(program)
    message = str(excinfo.value)
    assert expected_message in message
    assert f"line {expected_line}" in message


def test_structure_lines_keep_comments_and_block_numbers():
    """A comment after a structure keyword and a block number before one are
    still allowed; the head comment stays on the first body row of each pass."""
    df, _state = nc_to_dataframe(
        "R1=0\nN10 WHILE R1<2 ; head\nR1=R1+1\nX=R1\nN20 ENDWHILE\nIF R1==2 ; check\nY1\nELSE ; other\nY2\nENDIF"
    )
    assert df["X"].drop_nulls().to_list()[-1] == 2.0
    assert df["Y"].drop_nulls().to_list() == [1.0]
    assert df["comment"].drop_nulls().to_list() == ["; head", "; head", "; check"]
    assert df["N"].drop_nulls().to_list()[-1] == 20


def test_labelled_conditional_jump_keeps_its_label():
    """IF ... GOTO is a program jump, not a check structure, so its block may
    carry the label it jumps to."""
    df, _state = nc_to_dataframe("R1=0\nLB: R1=R1+1\nX=R1\nBACK: IF R1<3 GOTOB LB")
    assert df["X"].drop_nulls().to_list() == [1.0, 2.0, 3.0]


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
