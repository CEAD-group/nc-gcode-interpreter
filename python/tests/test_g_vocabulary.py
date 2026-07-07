"""G-command vocabulary lives in a Rust table (generated from ggroups.json),
not in the grammar: the grammar only recognizes lexical shapes."""

import pytest
from nc_gcode_interpreter import nc_to_dataframe


def test_keyword_g_commands_classify_to_their_group():
    df, _state = nc_to_dataframe("BSPLINE\nX1 Y1\nSOFT\nCP\nX2 Y2")
    assert df["gg01_motion"][0] == "BSPLINE"
    assert df["gg21_accel_profile"].drop_nulls()[0] == "SOFT"
    assert df["gg49_ptp_motion"].drop_nulls()[0] == "CP"


def test_lowercase_keyword_g_commands_work():
    # Case-insensitive language (manual: "No distinction is made between
    # uppercase and lowercase characters"); values normalize to uppercase.
    df, _state = nc_to_dataframe("bspline\nX1")
    assert df["gg01_motion"][0] == "BSPLINE"


def test_gframe_classifies():
    df, _state = nc_to_dataframe("GFRAME[42]\nX1")
    assert df["gg64_grinding_frames"][0] == "GFRAME[42]"


def test_unknown_gframe_index_errors():
    with pytest.raises(ValueError, match="not a G code"):
        nc_to_dataframe("GFRAME[101]\nX1")


def test_gframe_requires_a_separator_like_g_commands():
    """GFRAME[2]X10 must not tokenize; G1X10 is rejected the same way."""
    with pytest.raises(ValueError):
        nc_to_dataframe("GFRAME[2]X10\nX1")


def test_frame_keyword_outside_vocabulary_still_rejected_mid_block():
    """CROTS is not in the G-vocabulary table; it must still be rejected
    after another statement instead of falling through to a subprogram
    call that would silently move X to 0."""
    with pytest.raises(ValueError, match="frame instruction"):
        nc_to_dataframe("G1 CROTS X0")


def test_g_number_with_underscore_is_an_identifier():
    """G54_COMP must parse as a variable, not half-match as G54."""
    df, state = nc_to_dataframe("DEF REAL G54_COMP = 5\nX=G54_COMP")
    assert state["symbol_table"]["G54_COMP"] == 5.0
    assert df["X"][-1] == 5.0


def test_unknown_arithmetic_function_errors_with_context():
    with pytest.raises(ValueError, match="not a known arithmetic function"):
        nc_to_dataframe("X=NOPE(1)")


def test_lowercase_arithmetic_function_works():
    df, _state = nc_to_dataframe("X=sin(30)")
    assert abs(df["X"][0] - 0.5) < 1e-12
