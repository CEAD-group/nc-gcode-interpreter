"""Programmable frame rotations (ROT, AROT, RPL) on top of TRANS/ATRANS.

Conventions under test (NC programming manual 3.12.2.1 and 3.12.2.4, Basic
Functions manual 4.5.2.3 and 4.5.2.4): machine XYZ = R * local XYZ + t for the
geometry axes; ROT replaces the whole programmable frame, translation
included; AROT chains onto the current frame; several angles in one block are
RPY angles applied Z first, then Y', then X''; RPL turns about the normal of
the active plane; ATRANS after a rotation offsets in the rotated system.
"""

import math

import polars as pl
import pytest
from nc_gcode_interpreter import (
    dataframe_to_nc,
    nc_to_batches,
    nc_to_dataframe,
    nc_to_rows,
)
from nc_source import nc_lines
from polars.testing import assert_frame_equal


def xyz(df: pl.DataFrame, row: int = -1) -> tuple[float, float, float]:
    # A bare frame instruction block carries no cells and emits no row, so
    # row indices count motion (and commented) blocks only.
    return tuple(df[axis][row] for axis in ("X", "Y", "Z"))


def assert_xyz(df: pl.DataFrame, expected, row: int = -1, abs: float = 1e-9):
    assert xyz(df, row) == pytest.approx(expected, abs=abs)


def rot_z(deg: float):
    c, s = math.cos(math.radians(deg)), math.sin(math.radians(deg))
    return [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]


def apply(matrix, point):
    return tuple(sum(matrix[i][j] * point[j] for j in range(3)) for i in range(3))


def test_trans_then_arot_rotates_about_the_translated_origin():
    df, state = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "TRANS X10 Y20", "AROT Z90", "G1 X1 Y0")
    )
    # Local +X points along machine +Y after a +90 turn about Z.
    assert_xyz(df, (10.0, 21.0, 0.0))
    assert state["translation"]["X"] == 10.0 and state["translation"]["Y"] == 20.0
    for got, want in zip(state["rotation"], rot_z(90), strict=True):
        assert got == pytest.approx(want, abs=1e-12)


def test_rot_replaces_the_whole_frame_including_trans():
    """ROT is absolute: it deletes a previously programmed TRANS (3.12.2.1)."""
    df, state = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "TRANS X10", "ROT Z90", "G1 X1 Y0")
    )
    assert_xyz(df, (0.0, 1.0, 0.0))
    assert state["translation"]["X"] == 0.0


def test_trans_after_arot_deletes_the_rotation():
    """3.12.2.4 example 1 relies on TRANS resetting an earlier AROT."""
    df, state = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "AROT RPL=45", "TRANS X20 Y40", "G1 X1 Y0")
    )
    assert_xyz(df, (21.0, 40.0, 0.0))
    assert state["rotation"] == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]


def test_arot_composes_in_program_order():
    """AROT chains onto the rotated axes, so X-then-Y differs from Y-then-X."""
    x_then_y, _ = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "AROT X90", "AROT Y90", "G1 X1 Y2 Z3")
    )
    y_then_x, _ = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "AROT Y90", "AROT X90", "G1 X1 Y2 Z3")
    )
    # Rx(90) * Ry(90) * (1, 2, 3) and Ry(90) * Rx(90) * (1, 2, 3), by hand.
    assert_xyz(x_then_y, (3.0, 1.0, 2.0))
    assert_xyz(y_then_x, (2.0, -3.0, -1.0))


def test_rot_block_angles_follow_z_then_y_then_x():
    """Several angles in one block are RPY: Z first, then Y', then X''
    (3.12.2.4 "Rotation sequence"), which equals AROT Z, AROT Y, AROT X in
    that order and not the reverse."""
    one_block, _ = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "ROT X30 Y40 Z50", "G1 X1 Y2 Z3")
    )
    chained, _ = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "AROT Z50", "AROT Y40", "AROT X30", "G1 X1 Y2 Z3")
    )
    reversed_chain, _ = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "AROT X30", "AROT Y40", "AROT Z50", "G1 X1 Y2 Z3")
    )
    assert xyz(one_block) == pytest.approx(xyz(chained), abs=1e-9)
    assert xyz(one_block) != pytest.approx(xyz(reversed_chain), abs=1e-6)


@pytest.mark.parametrize(
    "plane, local, expected",
    [
        ("G17", "G1 X1 Y0 Z0", (0.0, 1.0, 0.0)),  # about Z: +X onto +Y
        ("G18", "G1 X0 Y0 Z1", (1.0, 0.0, 0.0)),  # about Y: +Z onto +X
        ("G19", "G1 X0 Y1 Z0", (0.0, 0.0, 1.0)),  # about X: +Y onto +Z
    ],
)
def test_rpl_rotates_about_the_active_plane_normal(plane, local, expected):
    df, _ = nc_to_dataframe(nc_lines(plane, "G1 X0 Y0 Z0", "AROT RPL=90", local))
    assert_xyz(df, expected)


def test_rpl_equals_rot_about_the_plane_normal():
    via_rpl, _ = nc_to_dataframe(nc_lines("G18", "G1 X0 Y0 Z0", "ROT RPL=33", "G1 X1 Y2 Z3"))
    via_axis, _ = nc_to_dataframe(nc_lines("G18", "G1 X0 Y0 Z0", "ROT Y33", "G1 X1 Y2 Z3"))
    assert xyz(via_rpl) == pytest.approx(xyz(via_axis), abs=1e-12)


def test_bare_rot_deletes_an_active_rotation():
    df, state = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "ROT Z90", "G1 X1 Y0", "ROT", "G1 X1 Y0")
    )
    assert_xyz(df, (0.0, 1.0, 0.0), row=1)
    assert_xyz(df, (1.0, 0.0, 0.0), row=2)
    assert state["rotation"] == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]


def test_bare_arot_is_a_noop():
    df, _ = nc_to_dataframe(nc_lines("G1 X0 Y0 Z0", "ROT Z90", "AROT", "G1 X1 Y0"))
    assert_xyz(df, (0.0, 1.0, 0.0))


def test_atrans_after_arot_offsets_in_the_rotated_system():
    """3.12.2.4 example 2 programs ATRANS X5 after AROT Y30: the additive
    offset refers to the last programmed (rotated) frame (3.12.2.1)."""
    df, state = nc_to_dataframe(
        nc_lines("G1 X0 Y0 Z0", "AROT Z90", "ATRANS X5", "G1 X0 Y0")
    )
    assert_xyz(df, (0.0, 5.0, 0.0))
    # The translation is reported in machine components.
    assert state["translation"]["X"] == pytest.approx(0.0, abs=1e-12)
    assert state["translation"]["Y"] == pytest.approx(5.0, abs=1e-12)


def test_ic_increments_the_local_coordinate_under_a_rotation():
    df, _ = nc_to_dataframe(
        nc_lines("G1 X1 Y0 Z0", "AROT Z90", "G1 X=IC(1)", "G1 Y=IC(2)")
    )
    # local (2, 0, 0) -> machine (0, 2, 0); local (2, 2, 0) -> machine (-2, 2, 0)
    assert_xyz(df, (0.0, 2.0, 0.0), row=1)
    assert_xyz(df, (-2.0, 2.0, 0.0), row=2)


def test_orientation_axes_are_never_rotated():
    df, _ = nc_to_dataframe(
        nc_lines(
            "G1 X1 Y0 Z0 A10 B20 C30",
            "AROT Z90",
            "G1 X1 A10 B20 C30",
            "ATRANS A5",
            "G1 A10",
        )
    )
    assert_xyz(df, (0.0, 1.0, 0.0), row=1)
    assert (df["A"][1], df["B"][1], df["C"][1]) == (10.0, 20.0, 30.0)
    # ATRANS on an orientation axis is a plain offset.
    assert df["A"][-1] == 15.0


def test_unpositioned_axis_coupled_by_the_rotation_is_emitted():
    """A 2D program tilted about X moves Z even though Z was never programmed;
    the row must report that motion (Z assumed at local 0)."""
    df, _ = nc_to_dataframe(nc_lines("G1 X0 Y1", "AROT X90", "G1 X1"))
    assert df["Y"][-1] == pytest.approx(0.0, abs=1e-12)
    assert df["Z"][-1] == pytest.approx(1.0, abs=1e-12)


def test_actual_machine_position_includes_the_rotation():
    _, state = nc_to_dataframe(
        nc_lines("G1 X1 Y0 Z0", "AROT Z90", "R1=$AA_IM[Y]", "R2=$AA_IW[Y]")
    )
    assert state["symbol_table"]["R1"] == pytest.approx(1.0, abs=1e-12)
    assert state["symbol_table"]["R2"] == 0.0


def test_arc_under_rpl_flattens_to_the_rotated_arc():
    """A G2 in the rotated G17 plane is still a machine-plane arc: the I/J
    centre offset rotates with the frame and the flattened samples lie on the
    rotated circle."""
    program = nc_lines("G17 G1 X0 Y0 Z0 F100", "AROT RPL=90", "G2 X10 Y0 I5 J0")
    baked = nc_lines("G17 G1 X0 Y0 Z0 F100", "G2 X0 Y10 I0 J5")
    raw, _ = nc_to_dataframe(program)
    assert raw["I"][-1] == pytest.approx(0.0, abs=1e-12)
    assert raw["J"][-1] == pytest.approx(5.0, abs=1e-12)
    flat, _ = nc_to_dataframe(program, flatten_tolerance=0.01)
    flat_baked, _ = nc_to_dataframe(baked, flatten_tolerance=0.01)
    assert flat.height > 10
    assert_frame_equal(
        flat.select("X", "Y", "Z"), flat_baked.select("X", "Y", "Z"), atol=1e-9
    )
    # The local arc (clockwise, through (5, 5)) maps to machine (-5, 5).
    assert flat["X"].min() == pytest.approx(-5.0, abs=0.05)


def test_cr_arc_under_an_in_plane_rotation_matches_the_baked_arc():
    program = nc_lines("G1 X0 Y0 Z0 F100", "AROT Z30", "G3 X10 Y0 CR=5")
    end = apply(rot_z(30), (10.0, 0.0, 0.0))
    baked = nc_lines("G1 X0 Y0 Z0 F100", f"G3 X{end[0]!r} Y{end[1]!r} CR=5")
    flat, _ = nc_to_dataframe(program, flatten_tolerance=0.01)
    flat_baked, _ = nc_to_dataframe(baked, flatten_tolerance=0.01)
    assert_frame_equal(
        flat.select("X", "Y", "Z"), flat_baked.select("X", "Y", "Z"), atol=1e-9
    )


@pytest.mark.parametrize(
    "arc",
    ["G2 X10 Y0 I5 J0", "G2 X10 Y0 CR=5", "G3 X10 Y0 Z2 I5 J0 TURN=1"],
    ids=lambda a: a.split()[-1],
)
def test_arc_under_a_tilting_rotation_errors(arc):
    """An arc whose plane no longer coincides with the machine plane is not
    modeled; it must fail instead of flattening a wrong arc."""
    with pytest.raises(ValueError, match="tilts the G17 plane"):
        nc_to_dataframe(nc_lines("G1 X0 Y0 Z0 F100", "AROT X30", arc))
    # A half turn keeps the plane parallel but reverses it: also unsupported.
    with pytest.raises(ValueError, match="tilts the G17 plane"):
        nc_to_dataframe(nc_lines("G1 X0 Y0 Z0 F100", "AROT Y180", arc))


def test_arc_in_another_plane_checks_that_plane():
    # In G18 the normal is Y, so an RPL (about Y) is fine and an X tilt is not.
    nc_to_dataframe(nc_lines("G18 G1 X0 Y0 Z0 F100", "AROT RPL=30", "G2 X10 Z0 I5 K0"))
    with pytest.raises(ValueError, match="tilts the G18 plane"):
        nc_to_dataframe(nc_lines("G18 G1 X0 Y0 Z0 F100", "AROT X30", "G2 X10 Z0 I5 K0"))


@pytest.mark.parametrize(
    "block, match",
    [
        ("ROT RPL=30 X10", "RPL= cannot be combined"),
        ("AROT A30", "geometry axes X, Y, Z only"),
        ("ROT RPL=\"A\"", "numeric angle"),
    ],
    ids=lambda v: v if v.startswith(("ROT", "AROT")) else "",
)
def test_unsupported_rotation_syntax_errors(block, match):
    with pytest.raises(ValueError, match=match):
        nc_to_dataframe(nc_lines("G1 X0 Y0 Z0", block, "G1 X1"))


def test_rotation_moving_an_unconfigured_geometry_axis_errors():
    # Without a Z axis a rotation about Z is representable, a tilt is not.
    df, _ = nc_to_dataframe(
        nc_lines("G1 X1 Y0", "AROT Z90", "G1 X1"), axis_identifiers=["X", "Y"]
    )
    assert (df["X"][-1], df["Y"][-1]) == pytest.approx((0.0, 1.0), abs=1e-12)
    with pytest.raises(ValueError, match="geometry axis Z, which is not a configured axis"):
        nc_to_dataframe(nc_lines("G1 X1 Y0", "AROT X30", "G1 X1"), axis_identifiers=["X", "Y"])


FRAME_PROGRAM = nc_lines(
    "G17 G1 X0 Y0 Z0 F100",
    "TRANS X10 Y20 Z5",
    "AROT Z30",
    "G1 X10 Y0",
    "G1 X10 Y10",
    "G1 X0 Y10 Z2",
    "G2 X0 Y0 I0 J-5",
    "G1 X=IC(3) Y=IC(-1)",
    "ATRANS Y1",
    "G1 X0 Y0 Z0",
    "M30",
)


def baked_program() -> str:
    """FRAME_PROGRAM with the frame multiplied into every coordinate."""
    matrix = rot_z(30)
    t = (10.0, 20.0, 5.0)

    def machine(local, offset=t):
        rotated = apply(matrix, local)
        return tuple(r + o for r, o in zip(rotated, offset, strict=True))

    def move(code, local, extra="", offset=t):
        x, y, z = machine(local, offset)
        return f"{code} X{x!r} Y{y!r} Z{z!r}{extra}"

    i, j, _ = apply(matrix, (0.0, -5.0, 0.0))
    # ATRANS Y1 after AROT Z30 shifts by R * (0, 1, 0).
    t2 = tuple(a + b for a, b in zip(t, apply(matrix, (0.0, 1.0, 0.0)), strict=True))
    return nc_lines(
        "G17 G1 X0 Y0 Z0 F100",
        move("G1", (10.0, 0.0, 0.0)),
        move("G1", (10.0, 10.0, 0.0)),
        move("G1", (0.0, 10.0, 2.0)),
        move("G2", (0.0, 0.0, 2.0), f" I{i!r} J{j!r}"),
        move("G1", (3.0, -1.0, 2.0)),
        move("G1", (0.0, 0.0, 0.0), offset=t2),
        "M30",
    )


def test_frame_program_matches_the_baked_coordinates():
    framed, _ = nc_to_dataframe(FRAME_PROGRAM, flatten_tolerance=0.01)
    baked, _ = nc_to_dataframe(baked_program(), flatten_tolerance=0.01)
    assert_frame_equal(
        framed.select("X", "Y", "Z"), baked.select("X", "Y", "Z"), atol=1e-9
    )


def test_export_and_reload_round_trip(tmp_path):
    """Output written back with dataframe_to_nc (which bakes the frame into
    the coordinates, rounded to 3 decimals) reloads to the same path."""
    framed, _ = nc_to_dataframe(FRAME_PROGRAM)
    exported = tmp_path / "baked.mpf"
    dataframe_to_nc(framed, exported)
    reloaded, state = nc_to_dataframe(exported.read_text())
    assert_frame_equal(
        framed.select("X", "Y", "Z", "I", "J"),
        reloaded.select("X", "Y", "Z", "I", "J"),
        atol=1e-3,
    )
    assert state["rotation"] == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]


def test_streaming_rows_and_batches_match_the_dataframe():
    df, state = nc_to_dataframe(FRAME_PROGRAM, include_line_numbers=True)
    rows = list(nc_to_rows(FRAME_PROGRAM))
    assert [line for line, _ in rows] == df["line_no"].to_list()
    streamed = pl.DataFrame(
        [row for _, row in rows],
        schema={name: df.schema[name] for name in df.columns if name != "line_no"},
    )
    assert_frame_equal(df.drop("line_no"), streamed, check_column_order=False)

    batches = nc_to_batches(FRAME_PROGRAM, batch_size=3, include_line_numbers=True)
    frames = list(batches)
    assert len(frames) > 1
    assert_frame_equal(
        pl.concat(frames, how="diagonal").select(df.columns), df, check_column_order=False
    )
    assert batches.state["rotation"] == state["rotation"]
    assert batches.state["translation"] == state["translation"]


def test_rotation_state_is_identity_without_frame_rotations():
    _, state = nc_to_dataframe("TRANS X10\nX1")
    assert state["rotation"] == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    assert state["translation"]["X"] == 10.0
