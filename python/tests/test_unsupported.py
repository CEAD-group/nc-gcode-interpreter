import pytest
from nc_gcode_interpreter import nc_to_dataframe


@pytest.mark.parametrize(
    "program",
    [
        "ROTS X30 Y10\nX2",  # two solid angles: the manuals disagree on the rotation
        "CROTS X30 Y10\nX2",
        "MIRROR A0\nX2",
        "SCALE X2\nAROT Z30\nX2",  # a rotation after unequal factors shears the frame
        "TOFRAME\nX2",  # frame rotations that follow the tool or workpiece orientation
        "TOFRAMEZ\nX2",
        "TOROT\nX2",
        "torotx\nX2",
        "PAROT\nX2",
        "G1 F100 X1 TOROT",
    ],
    ids=lambda p: " / ".join(p.splitlines()[:-1]) or p,
)
def test_unsupported_statements_error_loudly(program):
    """
    Statements that would silently corrupt the output (unmodeled frame
    transformations) must raise instead of being ignored.
    """
    with pytest.raises(ValueError, match=r"not supported|geometry axes X, Y, Z only"):
        nc_to_dataframe(program)


def test_switching_an_orientation_frame_off_is_accepted():
    df, _state = nc_to_dataframe("TOROTOF\nPAROTOF\nX2")
    assert df["gg53_frame_rot_tool"][-1] == "TOROTOF"
    assert df["gg52_frame_rot_wp"][-1] == "PAROTOF"


@pytest.mark.parametrize(
    "program",
    [
        "ROT\nX2",  # bare absolute frame instruction on an empty frame
        "MIRROR\nX2",
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


def test_trans_is_substituting():
    """TRANS deletes ALL previously programmed offsets, including on axes
    not mentioned in the block (manual 3.12.2.2)."""
    df, _state = nc_to_dataframe("TRANS X10\nX0 Y0\nTRANS Y5\nX0 Y0")
    assert df["X"].to_list() == [10.0, 0.0]
    assert df["Y"].to_list() == [0.0, 5.0]


def test_bare_absolute_frame_instruction_deletes_frame():
    """A bare ROT/SCALE/MIRROR deletes the whole programmable frame,
    including a previously set TRANS offset (manual 3.12.2.1)."""
    df, _state = nc_to_dataframe("TRANS X100\nX2\nROT\nX3")
    assert df["X"].to_list() == [102.0, 3.0]


def test_frame_instruction_mid_block_errors():
    """Frame instructions must be alone in the block; `G1 MIRROR X0` must
    not be interpreted as a G-command plus an axis move to X=0."""
    with pytest.raises(ValueError, match="not supported"):
        nc_to_dataframe("G1 F1000 MIRROR X0")


@pytest.mark.parametrize(
    "program",
    [
        "DEF REAL PW\nX1",  # bare definition
        "DEF REAL PW=1\nX1",  # definition with initialization
    ],
    ids=lambda p: p.splitlines()[0],
)
def test_def_of_block_address_errors(program):
    """PW/SD/PL are reserved block addresses; defining a variable with one
    of these names would silently shadow the output column."""
    with pytest.raises(ValueError, match="reserved block address"):
        nc_to_dataframe(program)


def test_lowercase_block_address_is_normalized():
    """Lowercase pw=2 must land in the PW column (and stay excluded from
    forward-fill), not create a separate lowercase column."""
    df, _state = nc_to_dataframe("BSPLINE F1000\nX=5 pw=2\nX=6")
    assert df["PW"].to_list()[-2:] == [2.0, None]
    assert "pw" not in df.columns


@pytest.mark.parametrize(
    "program, variable, expected",
    [
        ("DEF REAL TRANS_X = 5\nX=TRANS_X", "TRANS_X", 5.0),
        ("DEF REAL GOTOFFSET = 7\nX=GOTOFFSET", "GOTOFFSET", 7.0),
    ],
    ids=["TRANS_X", "GOTOFFSET"],
)
def test_keyword_prefixed_identifiers_still_parse(program, variable, expected):
    """Identifiers that merely start with a frame or GOTO keyword must
    still parse as ordinary variable names."""
    df, state = nc_to_dataframe(program)
    assert state["symbol_table"][variable] == expected
    assert df["X"][-1] == expected


def test_keyword_prefix_is_not_a_frame_instruction():
    """An identifier that merely starts with a frame keyword (e.g. a
    subprogram called TRANSFORM) must still parse as a function call."""
    df, _state = nc_to_dataframe("TRANSFORM(1)\nX2")
    assert df["non_returning_function_call"][0] == "TRANSFORM(1)"
