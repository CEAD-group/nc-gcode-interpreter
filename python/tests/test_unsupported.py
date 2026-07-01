import pytest
from nc_gcode_interpreter import nc_to_dataframe


@pytest.mark.parametrize(
    "program",
    [
        "GOTOF MYLABEL\nX2",  # jump forward to a label
        "GOTOF 20\nX2",  # jump forward to a block number
        "GOTOB START\nX2",  # jump backward
        "GOTO END1\nX2",  # jump
        "GOTOC TARGET\nX2",  # jump without alarm
        "LOOP\nX1\nENDLOOP",  # endless loop, can only be left with a jump
        "MIRROR X0\nX2",  # mirroring frame
        "AMIRROR X0\nX2",  # additive mirroring frame
        "ROT RPL=30\nX2",  # rotation frame
        "AROT RPL=30\nX2",  # additive rotation frame
        "SCALE X2 Y2\nX2",  # scaling frame
        "ASCALE X2 Y2\nX2",  # additive scaling frame
    ],
    ids=lambda p: p.splitlines()[0],
)
def test_unsupported_statements_error_loudly(program):
    """
    Statements that would silently corrupt the output (skipped jumps,
    unmodeled frame transformations) must raise instead of being ignored.
    """
    with pytest.raises(ValueError, match="not supported"):
        nc_to_dataframe(program)


@pytest.mark.parametrize(
    "program",
    [
        "ROT\nX2",  # bare substituting frame instruction: resets a frame
        "MIRROR\nX2",  # component this interpreter never sets -> safe no-op
        "SCALE\nX2",
    ],
    ids=lambda p: p.splitlines()[0],
)
def test_bare_frame_reset_is_noop(program):
    df, _state = nc_to_dataframe(program)
    assert df["X"][-1] == 2.0


def test_bare_trans_resets_translation():
    df, _state = nc_to_dataframe("X1\nTRANS X100\nX2\nTRANS\nX3")
    assert df["X"].to_list() == [1.0, 102.0, 3.0]


def test_keyword_prefix_is_not_a_frame_instruction():
    """An identifier that merely starts with a frame keyword (e.g. a
    subprogram called TRANSFORM) must still parse as a function call."""
    df, _state = nc_to_dataframe("TRANSFORM(1)\nX2")
    assert df["non_returning_function_call"][0] == "TRANSFORM(1)"
