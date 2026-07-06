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


def test_flattened_marker_column():
    df, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=0.1)
    assert "flattened" in df.columns
    # Unmarked rows are exactly the programmed positions: start, arc endpoint,
    # final G1 — and the marker is never forward-filled.
    originals = df.filter(pl.col("flattened").is_null())
    assert originals["X"].to_list() == [0.0, 100.0, 110.0]
    assert df.filter(pl.col("flattened") == 1.0).height == df.height - 3
    # Without flattening the column does not exist.
    df_plain, _ = nc_to_dataframe(ARC_PROGRAM)
    assert "flattened" not in df_plain.columns


def test_viz_toolpath_arrays():
    np = pytest.importorskip("numpy")
    from nc_gcode_interpreter.viz import FLATTENED_COLOR, PROGRAMMED_COLOR, toolpath_arrays

    df, _ = nc_to_dataframe(ARC_PROGRAM, flatten_tolerance=0.5)
    data, colors = toolpath_arrays(df, bead_width=4.0)
    assert data.shape == (df.height, 6)
    # Feed-based time: 1000 mm/min = 16.67 mm/s; the ~157 mm semicircle plus
    # 10 mm exit takes ~10 s.
    total_len = float(np.hypot(np.diff(data[:, 1]), np.diff(data[:, 2])).sum())
    assert abs(data[-1, 0] - total_len / (1000 / 60)) < 1e-3
    assert (np.diff(data[:, 0]) > 0).all()
    # Programmed vs generated coloring follows the flattened marker.
    marker = df["flattened"].fill_null(0.0).to_numpy().astype(bool)
    assert (colors[marker] == FLATTENED_COLOR).all()
    assert (colors[~marker] == PROGRAMMED_COLOR).all()

    # Without the marker column there is no color override.
    df_plain, _ = nc_to_dataframe(ARC_PROGRAM)
    _, colors_plain = toolpath_arrays(df_plain)
    assert colors_plain is None


def test_nc_view_cli(tmp_path, monkeypatch, capsys):
    pytest.importorskip("threejs_viewer")
    from nc_gcode_interpreter import cli, viz

    program = tmp_path / "demo.mpf"
    program.write_text(ARC_PROGRAM)

    captured = {}

    def fake_view(df, **kwargs):
        captured["df"] = df
        captured["kwargs"] = kwargs

    monkeypatch.setattr(viz, "view_toolpath", fake_view)
    assert cli.main([str(program), "--speed", "30", "--flatten-tolerance", "0.5"]) == 0
    assert captured["kwargs"]["speed"] == 30.0
    assert "flattened" in captured["df"].columns
    assert "from flattened curves" in capsys.readouterr().out

    # --no-flatten interprets without the marker column.
    monkeypatch.setattr(viz, "view_toolpath", fake_view)
    assert cli.main([str(program), "--no-flatten"]) == 0
    assert "flattened" not in captured["df"].columns


def test_nc_view_cli_axis_index_map(tmp_path, monkeypatch):
    pytest.importorskip("threejs_viewer")
    from nc_gcode_interpreter import cli, viz

    program = tmp_path / "flow.mpf"
    program.write_text("G1 X0 Y0 F1000\nFL[E]=10\nG1 X10 Y0\nX20 Y5\n")

    seen = {}
    monkeypatch.setattr(viz, "view_toolpath", lambda df, **kw: seen.setdefault("df", df))
    assert cli.main([str(program), "--axis-index-map", "E:8,X:0", "--no-flatten"]) == 0
    assert seen["df"].height >= 2

    with pytest.raises(SystemExit):
        cli.build_parser().parse_args(["x.mpf", "--axis-index-map", "not-a-map"])


def test_viz_bead_detection_and_travel():
    np = pytest.importorskip("numpy")
    from nc_gcode_interpreter.viz import detect_bead_size, toolpath_arrays

    program = (
        ";Layer height: 1.50\n"
        ";Deposition width: 6.00\n"
        "G1 X0 Y0 Z0 E0 F1000\n"
        "X10 Y0 E1\n"
        "X20 Y0 E2\n"
        "G0 X20 Y50\n"        # travel: E constant
        "G1 X30 Y50 E3\n"
    )
    df, _ = nc_to_dataframe(program)
    assert detect_bead_size(df) == (6.0, 1.5)

    data, _ = toolpath_arrays(df)
    widths = data[:, 4]
    # 7 points: 2 comment rows (at the forward-filled position), then the 5
    # motion blocks. Extruding points get the detected bead; the start cap,
    # comment rows and the travel move get width 0.
    assert widths.tolist() == [0.0, 0.0, 6.0, 6.0, 6.0, 0.0, 6.0]
    assert data[:, 5].max() == 1.5
    # Explicit arguments beat detection.
    data, _ = toolpath_arrays(df, bead_width=2.0, bead_height=1.0)
    assert data[:, 4].max() == 2.0

    # No E column: everything extrudes; no comments: fallback 4.0 x 2.0.
    df_plain, _ = nc_to_dataframe("G1 X0 Y0 F1000\nX10 Y0\nX20 Y5\n")
    data, _ = toolpath_arrays(df_plain)
    assert (data[:, 4] == 4.0).all() and (data[:, 5] == 2.0).all()
    assert detect_bead_size(df_plain) == (None, None)
