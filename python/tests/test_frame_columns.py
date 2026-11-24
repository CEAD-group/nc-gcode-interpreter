"""The programmable frame per row, `include_frame` (#130)."""

import math

import polars as pl
import pytest
from nc_gcode_interpreter import nc_to_batches, nc_to_dataframe

ROTATION = [f"frame_r{row}{column}" for row in (1, 2, 3) for column in (1, 2, 3)]
TRANSLATION = ["frame_tx", "frame_ty", "frame_tz"]
IDENTITY = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]


@pytest.fixture(params=["0", "1"])
def stage1(request, monkeypatch):
    monkeypatch.setenv("NC_STAGE1", request.param)


def frames(text, **options):
    df, state = nc_to_dataframe(text, include_frame=True, **options)
    return df, state


def rotation(df, row):
    return [df[name][row] for name in ROTATION]


def translation(df, row):
    return [df[name][row] for name in TRANSLATION]


def test_columns_are_absent_by_default(stage1):
    df, _ = nc_to_dataframe("AROT Z30\nG1 X1 Y0 Z0 F1000\n")
    assert not [name for name in df.columns if name.startswith("frame_")]


def test_issue_example_reports_the_evaluated_rotation(stage1):
    text = "G18\nR1=10\nAROT RPL=R1*3\nG1 X1 Y0 Z0 F1000\n"
    df, state = frames(text)
    row = df.height - 1
    assert rotation(df, row) == pytest.approx([v for line in state["rotation"] for v in line])
    # Machine XYZ of the row is R * local XYZ.
    r = rotation(df, row)
    assert [df["X"][row], df["Y"][row], df["Z"][row]] == pytest.approx([r[0], r[3], r[6]])
    assert r[0] == pytest.approx(math.cos(math.radians(30)))
    assert r[4] == pytest.approx(1.0)


def test_every_row_carries_the_frame_active_on_it(stage1):
    text = "G1 X1 Y0 Z0 F1000\nX2\nTRANS X10 Y20 Z30\nAROT Z90\nX1\nX2\nATRANS X1\nX3\nROT\nX4\nTRANS\nX5\n"
    df, _ = frames(text)
    quarter = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]
    # ATRANS X1 is added in the rotated frame; ROT deletes the whole frame.
    expected = [
        (IDENTITY, [0, 0, 0]),
        (IDENTITY, [0, 0, 0]),
        (quarter, [10, 20, 30]),
        (quarter, [10, 20, 30]),
        (quarter, [10, 21, 30]),
        (IDENTITY, [0, 0, 0]),
        (IDENTITY, [0, 0, 0]),
    ]
    assert df.height == len(expected)
    for row, (r, t) in enumerate(expected):
        assert rotation(df, row) == pytest.approx(r, abs=1e-12), row
        assert translation(df, row) == pytest.approx(t, abs=1e-12), row


def test_frame_from_a_loop_and_a_branch(stage1):
    text = "G1 X1 Y0 Z0 F1000\nR1=0\nWHILE R1<3\nAROT Z10\nX1\nR1=R1+1\nENDWHILE\nIF R1==3\nROT Z90\nENDIF\nX1\n"
    df, state = frames(text)
    angles = [math.degrees(math.atan2(df["frame_r21"][row], df["frame_r11"][row])) for row in range(df.height)]
    assert angles == pytest.approx([0, 10, 20, 30, 90])
    assert rotation(df, df.height - 1) == pytest.approx([v for line in state["rotation"] for v in line])


def test_only_changes_are_written_without_forward_fill(stage1):
    df, _ = frames("G1 X1 Y0 Z0 F1000\nX2\nAROT Z90\nX3\nX4\n", disable_forward_fill=True)
    assert df["frame_r11"].is_not_null().to_list() == [True, False, True, False]


def test_flattened_arc_rows_carry_the_frame(stage1):
    pytest.importorskip("numpy")
    text = "G1 X0 Y0 Z0 F1000\nAROT Z90\nG2 X10 Y0 I5 J0\nG1 X20\n"
    df, _ = frames(text, flatten_tolerance=0.01)
    assert df.height > 4
    assert df["frame_r21"].to_list() == pytest.approx([0.0] + [1.0] * (df.height - 1))


def test_batches_fill_the_frame_across_batch_borders(stage1):
    text = "G1 X0 Y0 Z0 F1000\nTRANS X5\n" + "".join(f"X{i}\n" for i in range(1, 10))
    whole, _ = frames(text)
    batches = pl.concat(list(nc_to_batches(text, batch_size=3, include_frame=True)))
    assert batches.select(ROTATION + TRANSLATION).equals(whole.select(ROTATION + TRANSLATION))
    assert batches["frame_tx"].to_list() == [0.0] + [5.0] * 9
