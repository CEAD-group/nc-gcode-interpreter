"""Programmable scaling, mirroring and solid-angle rotation.

Conventions under test (NC programming manual 3.12.2.5 to 3.12.2.7, Basic
Functions manual 4.5.2.11): machine XYZ = L * local XYZ + t with
L = rotation * scale * mirror; SCALE and MIRROR replace the whole programmable
frame, ASCALE and AMIRROR chain onto it; an offset programmed after a scale is
scaled; AMIRROR on a mirrored axis switches the mirror off (MD10612 = 0); one
solid angle is the same rotation as ROT about that axis.
"""

import math

import pytest
from nc_gcode_interpreter import nc_to_dataframe
from nc_source import nc_lines

IDENTITY = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
START = "G17 G1 X0 Y0 Z0 F1000"


@pytest.fixture(params=["0", "1"], autouse=True)
def stage1(request, monkeypatch):
    monkeypatch.setenv("NC_STAGE1", request.param)


def run(*lines, **options):
    return nc_to_dataframe(nc_lines(START, *lines), **options)


def xyz(df, row=-1):
    return [df[axis][row] for axis in ("X", "Y", "Z")]


def flat(matrix):
    return [value for row in matrix for value in row]


def test_scale_multiplies_the_programmed_coordinates():
    df, state = run("SCALE X2 Y3", "G1 X1 Y1 Z1")
    assert xyz(df) == pytest.approx([2.0, 3.0, 1.0])
    assert state["rotation"] == [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 1.0]]


def test_scale_is_absolute_and_ascale_is_additive():
    df, _ = run("TRANS X10", "SCALE X2", "G1 X1", "ASCALE X3", "G1 X1", "SCALE X5", "G1 X1")
    # SCALE deletes the TRANS; ASCALE multiplies the factor; SCALE replaces it.
    assert df["X"].to_list()[1:] == pytest.approx([2.0, 6.0, 5.0])


def test_scale_does_not_move_the_machine():
    df, state = run("G1 X4 Y6", "SCALE X2 Y3", "G1 Z1")
    assert xyz(df) == pytest.approx([4.0, 6.0, 1.0])
    assert state["axes"]["X"] == pytest.approx(2.0)
    assert state["axes"]["Y"] == pytest.approx(2.0)


def test_offset_after_a_scale_is_scaled():
    # Manual 3.12.2.6, note "Work offset after scaling".
    df, state = run("TRANS X10", "ASCALE X2", "ATRANS X5", "G1 X1")
    assert df["X"][-1] == pytest.approx(10.0 + 2.0 * (5.0 + 1.0))
    assert state["translation"]["X"] == pytest.approx(20.0)


def test_manual_example_offset_rotation_and_scale():
    # Manual 3.12.2.6 example 2: TRANS X40 Y20, AROT RPL=35, ASCALE X0.7 Y0.7.
    df, _ = run("TRANS X40 Y20", "AROT RPL=35", "ASCALE X0.7 Y0.7", "G1 X10 Y5")
    c, s = math.cos(math.radians(35)), math.sin(math.radians(35))
    assert xyz(df)[:2] == pytest.approx([40 + 0.7 * (10 * c - 5 * s), 20 + 0.7 * (10 * s + 5 * c)])


def test_rotation_after_equal_factors_is_accepted():
    df, _ = run("SCALE X2 Y2", "AROT Z90", "G1 X1 Y0")
    assert xyz(df)[:2] == pytest.approx([0.0, 2.0], abs=1e-12)


@pytest.mark.parametrize("block", ["SCALE X0", "SCALE X-1", "ASCALE Y0"])
def test_scale_factor_must_be_positive(block):
    with pytest.raises(ValueError, match="must be greater than 0"):
        run(block, "G1 X1")


def test_mirror_reverses_the_programmed_axis():
    df, state = run("MIRROR X0", "G1 X1 Y2 Z3")
    assert xyz(df) == pytest.approx([-1.0, 2.0, 3.0])
    assert state["rotation"] == [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]


def test_mirror_value_is_not_evaluated():
    df, _ = run("MIRROR X1 Y=5", "G1 X1 Y2")
    assert xyz(df)[:2] == pytest.approx([-1.0, -2.0])


def test_manual_example_four_mirrored_contours():
    # Manual 3.12.2.7 example 1: MIRROR X0, AMIRROR Y0, MIRROR Y0, MIRROR.
    df, state = run(
        "G1 X1 Y2", "MIRROR X0", "G1 X1 Y2", "AMIRROR Y0", "G1 X1 Y2", "MIRROR Y0", "G1 X1 Y2", "MIRROR", "G1 X1 Y2"
    )
    points = list(zip(df["X"].to_list()[1:], df["Y"].to_list()[1:], strict=True))
    assert points == [(1.0, 2.0), (-1.0, 2.0), (-1.0, -2.0), (1.0, -2.0), (1.0, 2.0)]
    assert state["rotation"] == IDENTITY


def test_second_amirror_switches_the_mirror_off():
    df, state = run("MIRROR X0", "AMIRROR X0", "G1 X1")
    assert df["X"][-1] == pytest.approx(1.0)
    assert state["rotation"] == IDENTITY


def test_manual_example_mirror_then_offset():
    # Manual 3.12.2.7 example 2: TRANS X0 Z600, AMIRROR Z0, ATRANS Z120.
    df, _ = run("TRANS X0 Z600", "AMIRROR Z0", "ATRANS Z120", "G1 Z10")
    assert df["Z"][-1] == pytest.approx(600.0 - 120.0 - 10.0)


def test_rotation_after_a_mirror_turns_the_other_way():
    # Manual 3.12.2.7, note on AROT after MIRROR: Mx * Rz(a) = Rz(-a) * Mx.
    mirrored_first, _ = run("MIRROR X0", "AROT Z30", "G1 X1 Y2 Z3")
    rotated_first, _ = run("ROT Z-30", "AMIRROR X0", "G1 X1 Y2 Z3")
    assert xyz(mirrored_first) == pytest.approx(xyz(rotated_first), abs=1e-12)
    same_sign, _ = run("ROT Z30", "AMIRROR X0", "G1 X1 Y2 Z3")
    assert xyz(mirrored_first) != pytest.approx(xyz(same_sign), abs=1e-6)


@pytest.mark.parametrize("op", ["ROTS", "CROTS"])
@pytest.mark.parametrize("word", ["X30", "Y-40", "Z=25"])
def test_one_solid_angle_equals_rot(op, word):
    solid, state = run("TRANS X10", f"{op} {word}", "G1 X1 Y2 Z3")
    plain, expected = run("TRANS X10", f"ROT {word}", "G1 X1 Y2 Z3")
    assert xyz(solid) == pytest.approx(xyz(plain), abs=1e-12)
    assert flat(state["rotation"]) == pytest.approx(flat(expected["rotation"]), abs=1e-12)


def test_arots_chains_onto_the_frame():
    solid, _ = run("TRANS X10", "AROT Z20", "AROTS X30", "G1 X1 Y2 Z3")
    plain, _ = run("TRANS X10", "AROT Z20", "AROT X30", "G1 X1 Y2 Z3")
    assert xyz(solid) == pytest.approx(xyz(plain), abs=1e-12)


def test_three_solid_angles_are_rejected():
    with pytest.raises(ValueError, match="at most two solid angles"):
        run("ROTS X10 Y10 Z10", "G1 X1")


@pytest.mark.parametrize("block", ["ASCALE", "AMIRROR", "AROTS"])
def test_bare_additive_instruction_keeps_the_frame(block):
    df, _ = run("TRANS X10", block, "G1 X5")
    assert df["X"][-1] == pytest.approx(15.0)


@pytest.mark.parametrize("block", ["SCALE", "MIRROR", "ROTS", "CROTS"])
def test_bare_absolute_instruction_deletes_the_frame(block):
    df, state = run("TRANS X10", "ASCALE X2", block, "G1 X5")
    assert df["X"][-1] == pytest.approx(5.0)
    assert state["rotation"] == IDENTITY


def test_combined_chain_matches_the_composed_matrices():
    df, state = run("TRANS X10 Y20", "AROT Z90", "ASCALE X2 Y2 Z3", "AMIRROR Y0", "ATRANS X1 Y1 Z1", "G1 X1 Y1 Z1")
    # L = Rz(90) * diag(2, 2, 3) * diag(1, -1, 1), t = (10, 20, 0) + L * (1, 1, 1).
    assert flat(state["rotation"]) == pytest.approx([0, 2, 0, 2, 0, 0, 0, 0, 3], abs=1e-12)
    assert xyz(df) == pytest.approx([10 + 2 * 2, 20 + 2 * 2, 3 * 2], abs=1e-12)


def test_frame_columns_hold_scale_and_mirror():
    df, _ = run("SCALE X2", "AMIRROR Y0", "G1 X1 Y1", include_frame=True)
    row = df.row(-1, named=True)
    assert [row["frame_r11"], row["frame_r22"], row["frame_r33"]] == [2.0, -1.0, 1.0]


def test_final_state_with_scale_and_mirror_resumes():
    _, state = run("TRANS X10", "ASCALE X2", "AMIRROR Y0", "G1 X1 Y1")
    df, _ = nc_to_dataframe("G1 X3 Y3", start=state)
    assert (df["X"][-1], df["Y"][-1]) == pytest.approx((16.0, -3.0))


def test_arc_under_a_uniform_scale_scales_centre_and_radius():
    df, _ = run("SCALE X2 Y2", "G1 X1 Y0", "G2 X3 Y0 I1 J0", "G2 X5 Y0 CR=1")
    assert (df["I"][-2], df["J"][-2]) == pytest.approx((2.0, 0.0))
    assert df["X"][-2] == pytest.approx(6.0)
    assert df["CR"][-1] == pytest.approx(2.0)


@pytest.mark.parametrize(
    "frame",
    ["SCALE X2", "SCALE X2 Y3", "MIRROR X0", "MIRROR Y0"],
)
def test_arc_under_a_distorting_or_mirroring_frame_errors(frame):
    with pytest.raises(ValueError, match="tilts, mirrors or unevenly scales the G17 plane"):
        run(frame, "G1 X1 Y0", "G2 X3 Y0 I1 J0")


def test_arc_ignores_scale_and_mirror_of_the_plane_normal():
    df, _ = run("SCALE Z3", "AMIRROR Z0", "G1 X1 Y0 Z1", "G2 X3 Y0 I1 J0")
    assert (df["X"][-1], df["I"][-1], df["Z"][-1]) == pytest.approx((3.0, 1.0, -3.0))


def test_scaled_square_program():
    # Manual 3.12.2.6: the contour is enlarged by the factors of the programmed axes.
    square = ["G1 X10 Y0", "G1 X10 Y10", "G1 X0 Y10", "G1 X0 Y0"]
    df, _ = run("TRANS X5 Y5", "ASCALE X2 Y2", *square)
    points = list(zip(df["X"].to_list()[-4:], df["Y"].to_list()[-4:], strict=True))
    assert points == pytest.approx([(25, 5), (25, 25), (5, 25), (5, 5)])


def test_scale_applies_to_a_non_geometry_axis():
    # Manual 3.12.2.6 scales all path, synchronized and positioning axes, not only X, Y, Z.
    df, state = run("G1 E0", "SCALE E1.5", "G1 X1 E10", "G1 E=IC(2)", "G1 E4")
    assert df["E"].to_list()[-3:] == pytest.approx([15.0, 18.0, 6.0])
    assert df["X"][-3] == pytest.approx(1.0)
    assert state["axes"]["E"] == pytest.approx(4.0)


def test_ascale_multiplies_and_bare_scale_resets_a_non_geometry_axis():
    df, _ = run("SCALE E2", "G1 E1", "ASCALE E3", "G1 E1", "SCALE", "G1 E1")
    assert df["E"].to_list()[-3:] == pytest.approx([2.0, 6.0, 1.0])


def test_offset_after_a_scale_is_scaled_on_a_non_geometry_axis():
    df, state = run("TRANS E10", "ASCALE E2", "ATRANS E5", "G1 E1")
    assert df["E"][-1] == pytest.approx(10.0 + 2.0 * (5.0 + 1.0))
    assert state["translation"]["E"] == pytest.approx(20.0)


def test_scale_does_not_move_a_non_geometry_axis():
    df, state = run("G1 E6", "SCALE E2", "G1 X1")
    assert df["E"][-1] == pytest.approx(6.0)
    assert state["axes"]["E"] == pytest.approx(3.0)


def test_scale_of_an_unknown_axis_errors():
    with pytest.raises(ValueError, match="not a configured axis"):
        run("SCALE ELX=2", "G1 X1")


def test_final_state_with_a_non_geometry_scale_resumes():
    _, state = run("TRANS E1", "ASCALE E2", "G1 E3")
    assert state["axis_scale"] == {"E": pytest.approx(2.0)}
    df, _ = nc_to_dataframe("G1 E4", start=state)
    assert df["E"][-1] == pytest.approx(9.0)


def test_start_axis_scale_rejects_a_geometry_axis():
    with pytest.raises(ValueError, match="start\\['rotation'\\]"):
        nc_to_dataframe("G1 X1 F100", start={"axis_scale": {"X": 2.0}})
