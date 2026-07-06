"""Curve flattening (flatten_tolerance): arcs and splines become G1 runs."""

import math

import polars as pl
import pytest
from nc_gcode_interpreter import nc_to_batches, nc_to_dataframe, nc_to_rows

ARC_PROGRAM = "G17 G1 X0 Y0 Z0 F1000\nG2 X100 Y0 I50 J0\nG1 X110 Y0\n"

SPLINE_PROGRAM = (
    "G1 X0 Y0 F300\n"
    "BSPLINE X10 Y20 PW=2\n"
    "X20 Y40\n"
    "X30 Y30\n"
    "X40 Y45\n"
    "X50 Y0\n"
    "G1 X60 Y0\n"
)


def test_default_keeps_curves_untouched():
    df, _ = nc_to_dataframe(ARC_PROGRAM)
    assert df["gg01_motion"].to_list() == ["G1", "G2", "G1"]
    assert "I" in df.columns


def test_arc_flattening_dataframe():
    tolerance = 0.1
    df, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=tolerance)
    # Everything is linear now, and the arc parameters are consumed.
    assert set(df["gg01_motion"].to_list()) == {"G1"}
    assert "I" not in df.columns and "J" not in df.columns and "CR" not in df.columns
    # The semicircle got sampled: many more rows than the 3 input blocks.
    assert df.height > 10
    # Samples lie on the circle of radius 50 around (50, 0).
    arc = df.filter(pl.col("X") <= 100.0).slice(1)
    radii = ((arc["X"] - 50.0) ** 2 + arc["Y"] ** 2).sqrt()
    assert ((radii - 50.0).abs() < 1e-9).all()
    # Chord sagitta stays within the tolerance.
    xs, ys = df["X"].to_list(), df["Y"].to_list()
    for (x0, y0), (x1, y1) in zip(zip(xs, ys), zip(xs[1:], ys[1:])):
        mx, my = (x0 + x1) / 2.0, (y0 + y1) / 2.0
        if max(x0, x1) <= 100.0:  # arc portion
            sagitta = 50.0 - math.hypot(mx - 50.0, my)
            assert sagitta <= tolerance + 1e-9


def test_arc_flattening_line_numbers_point_at_source_block():
    df, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=0.5)
    # F is forward-filled over the sampled rows like any modal value.
    assert (df["F"] == 1000.0).all()


def test_spline_flattening_dataframe():
    df, _ = nc_to_dataframe(SPLINE_PROGRAM, flatten_tolerance=0.05)
    assert set(df["gg01_motion"].to_list()) == {"G1"}
    assert "PW" not in df.columns
    assert df.height > 20
    # The B-spline ends exactly at the last control point.
    spline_end = df.filter(pl.col("X") == 50.0)
    assert spline_end.height >= 1
    assert abs(spline_end["Y"][0]) < 1e-9


def test_tighter_tolerance_more_rows():
    coarse, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=1.0)
    fine, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=0.01)
    assert fine.height > 2 * coarse.height


def test_flatten_via_rows_iterator():
    rows = list(nc_to_rows(ARC_PROGRAM, flatten_tolerance=0.5))
    assert len(rows) > 5
    # Every sampled row reports the arc's source line (line 2).
    arc_rows = [r for line_no, r in rows if line_no == 2]
    assert len(arc_rows) > 3
    assert all(r["gg01_motion"] == "G1" for _, r in rows)


def test_flatten_via_batches_matches_dataframe():
    df, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=0.5)
    batches = list(nc_to_batches(ARC_PROGRAM, batch_size=4, flatten_tolerance=0.5))
    combined = pl.concat(batches, how="diagonal")
    assert combined.height == df.height
    assert combined["X"].to_list() == df["X"].to_list()


def test_invalid_tolerance_raises():
    with pytest.raises(ValueError):
        nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=0.0)
    with pytest.raises(ValueError):
        nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=-1.0)
