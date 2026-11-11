"""Alarm 10860: a block that traverses a path needs a programmed feed.

The control also deletes F when the feed type changes (G93/G94/G95,
G96/G961/G962), so F has to be programmed again after such a change.
"""

import itertools

import pytest
from nc_gcode_interpreter import NcError, nc_to_batches, nc_to_dataframe, nc_to_rows

FLAT = "G0 X0 Y0\nN20 G1 X10\n"
STRUCTURED = "G0 X0\nR1 = 0\nWHILE R1 < 2\nR1 = R1 + 1\nENDWHILE\nG2 X10 Y0 I5 J0\n"
AFTER_FEED_TYPE = "G1 G94 F500 X1\nG95 X2\n"
IN_SUBPROGRAM = "G0 X0\nMOVE\n"
MOVE = "PROC MOVE\nG0 X1\nG1 X2\nM17\n"


@pytest.mark.parametrize(
    "program",
    ["G1 X1", "G2 X1 Y1 I1 J0", "G3 X1 Y1 CR=1", "CIP X1 Y1", "BSPLINE X1", "G1\nY1"],
)
def test_path_motion_without_feed_is_an_error(program):
    with pytest.raises(NcError) as info:
        nc_to_dataframe(program)
    assert info.value.kind == "feed_not_programmed"
    assert "no feed rate F has been programmed" in str(info.value)


@pytest.mark.parametrize(
    "program",
    [
        "G0 X1",
        "G4 F2",
        "G33 Z10 K2",
        "G74 X1",
        "G1",
        "G1 F100 X1\nG93 X2",
        "G94 F100\nG1 X1",
    ],
)
def test_no_error_without_a_path_or_with_a_feed(program):
    nc_to_dataframe(program)


@pytest.mark.parametrize(
    ("switch", "code"), [("G94 F500 X1\nG95", "G95"), ("G96 S100 F1 X1\nG961", "G961")]
)
def test_a_feed_type_change_deletes_the_feed(switch, code):
    with pytest.raises(NcError, match=f"feed type changed to {code}") as info:
        nc_to_dataframe(f"G1 {switch} X2\n")
    assert info.value.line == 2


def test_an_unknown_previous_feed_type_does_not_delete_the_feed():
    # The power-on feed type is machine data; without G93/G94/G95 programmed
    # or passed in start["g_groups"], the interpreter does not guess it.
    nc_to_dataframe("G1 F500 X1\nG95 X2\n")


def test_reprogramming_f_with_the_feed_type_change_is_fine():
    df, state = nc_to_dataframe("G1 F500 X1\nG95 F0.2 X2\n")
    assert df["F"].to_list() == [500.0, 0.2]
    assert state["feed"] == 0.2


def test_start_feed_is_the_default_feed():
    df, _ = nc_to_dataframe("G1 X1", start={"feed": 2000.0})
    assert df["F"].to_list() == [2000.0]


def test_no_check_when_f_is_not_a_configured_axis():
    nc_to_dataframe("G1 X1", axis_identifiers=["X", "Y"])


def error_of(call):
    with pytest.raises(NcError) as info:
        call()
    return str(info.value), info.value.kind, info.value.line


@pytest.mark.parametrize(
    ("program", "subprograms", "line", "kind"),
    [
        (FLAT, None, 2, "feed_not_programmed"),
        (STRUCTURED, None, 6, "feed_not_programmed"),
        (AFTER_FEED_TYPE, None, 2, "feed_not_programmed"),
        # Errors inside a subprogram are wrapped in the context of the call.
        (IN_SUBPROGRAM, {"MOVE": MOVE}, 2, "parse_context"),
    ],
)
def test_the_error_is_identical_on_every_execution_path(
    monkeypatch, program, subprograms, line, kind
):
    seen = set()
    for stage1, vm, stream in itertools.product("01", "01", ("0", None)):
        monkeypatch.setenv("NC_STAGE1", stage1)
        monkeypatch.setenv("NC_VM", vm)
        if stream is None:
            monkeypatch.delenv("NC_STREAM_MIN_BYTES", raising=False)
        else:
            monkeypatch.setenv("NC_STREAM_MIN_BYTES", stream)
        seen.add(error_of(lambda: nc_to_dataframe(program, subprograms=subprograms)))
        seen.add(
            error_of(
                lambda: list(
                    nc_to_batches(program, subprograms=subprograms, batch_size=1)
                )
            )
        )
        if subprograms is None:
            seen.add(error_of(lambda: list(nc_to_rows(program))))
    assert len(seen) == 1, seen
    ((message, *where),) = seen
    assert where == [kind, line]
    assert "feed rate F" in message or "F was not programmed" in message
