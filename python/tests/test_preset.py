"""PRESETON and PRESETONS set an axis's actual value (#122, manual 4.8.5).

The axis does not move. The row of the call carries the new value, and later
absolute and incremental values start from it.
"""

import itertools

import polars as pl
import pytest
from nc_gcode_interpreter import NcError, nc_to_batches, nc_to_dataframe

RESET_PROGRAM = "G1 X300 E=10 F3000\nPRESETON(E,0)\nG1 Y300 E=4\n"


@pytest.fixture(params=["0", "1"])
def stage1(request, monkeypatch):
    monkeypatch.setenv("NC_STAGE1", request.param)


def column(text: str, name: str, **kwargs) -> list:
    df, _ = nc_to_dataframe(text, extra_axes=["E"], **kwargs)
    return df[name].to_list()


def test_absolute_value_after_reset(stage1):
    assert column(RESET_PROGRAM, "E") == [10.0, 0.0, 4.0]
    assert column(RESET_PROGRAM, "non_returning_function_call") == [None, "PRESETON(E,0)", None]


def test_preset_before_the_first_value(stage1, capfd):
    assert column("PRESETON(E,10)\nG1 X300 E=IC(4.5) F3000\n", "E") == [10.0, 14.5]
    assert column("PRESETON(E,0)\nG1 X300 E=IC(4.5) F3000\n", "E") == [0.0, 4.5]
    assert "before any absolute position" not in capfd.readouterr().err


def test_several_axes_and_expressions(stage1):
    text = "R1=3\nN10 PRESETON(X, 5, E, R1+2)\nG1 X=IC(1) E=IC(1) F100\n"
    assert column(text, "X")[-2:] == [5.0, 6.0]
    assert column(text, "E")[-2:] == [5.0, 6.0]


def test_presetons_and_lower_case(stage1):
    assert column("G1 E=10 F100\npresetons(e,2)\nE=IC(1)\n", "E") == [10.0, 2.0, 3.0]


def test_preset_is_a_machine_value_under_a_frame(stage1):
    # TRANS stays active: X0 after the preset is again 10 in machine terms.
    text = "TRANS X10\nG1 X0 F100\nPRESETON(X,50)\nX=IC(1)\nX0\n"
    assert column(text, "X")[-4:] == [10.0, 50.0, 51.0, 10.0]


def test_batches_match_the_dataframe(stage1):
    frames = list(nc_to_batches(RESET_PROGRAM, extra_axes=["E"], batch_size=2))
    assert pl.concat(frames, how="diagonal")["E"].to_list() == [10.0, 0.0, 4.0]


def test_flattened_arc_starts_from_the_preset(stage1):
    text = "G0 X0 Y0 Z1\nPRESETON(E,0)\nG2 X200 Y0 I100 J0 E=4 F3000\n"
    values = column(text, "E", flatten_tolerance=0.5)[2:]
    steps = [b - a for a, b in itertools.pairwise([0.0, *values])]
    assert values[-1] == pytest.approx(4.0)
    assert max(steps) == pytest.approx(min(steps))


def test_unconfigured_axis_warns_and_is_not_applied(stage1, capfd):
    df, _ = nc_to_dataframe("PRESETON(Q,1)\n")
    assert "Q" not in df.columns
    assert "not a configured axis" in capfd.readouterr().err


@pytest.mark.parametrize(
    "call",
    ["PRESETON(E)", "PRESETON()", "PRESETON(E,1,X)", 'PRESETON(E,"A")', "PRESETON(X,,2,E,3)", "PRESETON(,X,2)"],
)
def test_malformed_calls_fail(stage1, call):
    with pytest.raises(NcError):
        nc_to_dataframe(call + "\n", extra_axes=["E"])


def test_geometry_axis_under_rotation_fails(stage1):
    with pytest.raises(NcError, match="rotation"):
        nc_to_dataframe("ROT Z30\nG1 X1 Y1 F100\nPRESETON(X,0)\n")


def test_preset_ends_a_pending_spline_at_its_programmed_endpoint(stage1):
    """The preset is not a control point: the spline ends at X20 Y0 before it."""
    text = "G1 X0 Y0 F100\nBSPLINE SD=2\nX10 Y10\nX20 Y0\nPRESETON(X,100)\nG1 X101 Y0\n"
    df, _ = nc_to_dataframe(text, flatten_tolerance=0.5)
    rows = list(zip(df["X"].to_list(), df["Y"].to_list(), df["non_returning_function_call"].to_list(), strict=True))
    preset = next(i for i, row in enumerate(rows) if row[2] is not None)
    assert rows[preset - 1][:2] == (20.0, 0.0)
    assert rows[preset][:2] == (100.0, 0.0)
    assert rows[preset + 1][:2] == (101.0, 0.0)
    assert max(x for x, _, _ in rows[:preset]) == 20.0


def test_spline_after_a_preset_starts_from_the_preset_value(stage1):
    text = "G1 X0 Y0 F100\nBSPLINE SD=2\nX10 Y10\nX20 Y0\nPRESETON(X,100)\nX110 Y10\nX120 Y0\nG1 X130\n"
    df, _ = nc_to_dataframe(text, flatten_tolerance=0.5)
    xs = df["X"].to_list()
    preset = df["non_returning_function_call"].to_list().index("PRESETON(X,100)")
    assert all(100.0 <= x <= 130.0 for x in xs[preset:])
    assert xs[preset + 1] - 100.0 < 10.0


def test_preset_under_g1_without_feed_is_not_a_move(stage1):
    df, _ = nc_to_dataframe("G1\nPRESETON(X,50)\n")
    assert df["X"].to_list()[-1] == 50.0
    with pytest.raises(NcError):
        nc_to_dataframe("G1\nPRESETON(X,50)\nX51\n")
