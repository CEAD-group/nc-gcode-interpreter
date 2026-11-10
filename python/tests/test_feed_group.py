"""FGROUP and FGREF as modal state (#107, manual 3.7.1).

FGROUP(<axes>) sets the channel axes the path feed F refers to; FGROUP()
restores the machine-data default, which the interpreter cannot know and
writes as "". FGREF[<axis>] is the reference radius of an axis in that path.
Both are forward-filled columns; neither is a call or a variable.
"""

import os

import polars as pl
import pytest
from nc_gcode_interpreter import NcError, nc_to_batches, nc_to_dataframe, nc_to_rows
from polars.testing import assert_frame_equal

ISSUE_PROGRAM = (
    "G1 X0 Y0 F1000\n"
    "FGROUP(RA1,RA2,RA3,RA4,RA5,RA6)\n"
    "FGREF[RA1]=300\n"
    "G1 X10\n"
    "FGROUP()\n"
    "X20\n"
)
ALL_RA = "RA1,RA2,RA3,RA4,RA5,RA6"


def test_issue_program():
    df, state = nc_to_dataframe(ISSUE_PROGRAM, axis_index_map={"RA1": 1})
    assert "non_returning_function_call" not in df.columns
    assert df["FGROUP"].to_list() == [None, ALL_RA, ALL_RA, ALL_RA, "", ""]
    assert df["FGREF[RA1]"].to_list() == [None, None, 300.0, 300.0, 300.0, 300.0]
    assert df.schema["FGROUP"] == pl.String
    assert df.schema["FGREF[RA1]"] == pl.Float64
    assert not any(k.startswith("FGREF") for k in state["symbol_table"])


def test_fgref_needs_no_axis_index_map():
    df, _ = nc_to_dataframe("FGREF[RA2]=12.5\nX1\n")
    assert df["FGREF[RA2]"].to_list() == [12.5, 12.5]


def test_columns_absent_when_unused():
    df, _ = nc_to_dataframe("G1 X0 F100\nX1\n")
    assert not [c for c in df.columns if c.startswith("FG")]


def test_column_order_after_axes():
    df, _ = nc_to_dataframe("FGREF[RA1]=1\nFGREF[A]=2\nFGROUP(X,A)\nG1 X1 A2 F100\n")
    assert df.columns[-3:] == ["FGROUP", "FGREF[A]", "FGREF[RA1]"]


@pytest.mark.parametrize(
    "program",
    ["FGROUP(X,NOPE)\n", "FGROUP(1)\n", "FGREF[NOPE]=1\n", "FGREF[1]=1\n"],
)
def test_unknown_axis_errors(program):
    with pytest.raises(NcError) as exc:
        nc_to_dataframe(program)
    assert exc.value.kind == "unexpected_axis"
    assert exc.value.line == 1


def test_stage1_fast_path_agrees():
    """FGROUP() would decode as a parameterless call on the streaming fast path."""
    program = "G1 X0 F100\nFGROUP(X,RA1)\nX1\nFGROUP()\nX2\n"
    previous = os.environ.get("NC_STAGE1")
    try:
        os.environ["NC_STAGE1"] = "0"
        full, _ = nc_to_dataframe(program)
        os.environ["NC_STAGE1"] = "1"
        fast, _ = nc_to_dataframe(program)
    finally:
        if previous is None:
            os.environ.pop("NC_STAGE1", None)
        else:
            os.environ["NC_STAGE1"] = previous
    assert_frame_equal(full, fast)
    assert fast["FGROUP"].to_list() == [None, "X,RA1", "X,RA1", "", ""]


def test_forward_fill_carries_across_batches():
    df, _ = nc_to_dataframe(ISSUE_PROGRAM)
    frames = list(nc_to_batches(ISSUE_PROGRAM, batch_size=2))
    assert_frame_equal(pl.concat(frames, how="diagonal").select(df.columns), df)


def test_rows_carry_feed_group():
    rows = [row for _, row in nc_to_rows(ISSUE_PROGRAM)]
    assert rows[-1]["FGROUP"] == ""
    assert rows[-1]["FGREF[RA1]"] == 300.0


def test_flattened_arc_keeps_feed_group_on_first_sample():
    df, _ = nc_to_dataframe(
        "G1 X0 Y0 F100\nFGROUP(X,Y) G2 X10 Y0 I5 J0\n", flatten_tolerance=0.1
    )
    assert df["FGROUP"].to_list()[0] is None
    assert set(df["FGROUP"].to_list()[1:]) == {"X,Y"}
