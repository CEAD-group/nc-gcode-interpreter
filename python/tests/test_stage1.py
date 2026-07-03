"""Differential tests for the stage-1 line triage: every program must
produce identical output with the fast path enabled (default) and disabled
(NC_STAGE1=0, whole-file parse). The fast path only exists because it is
provably equivalent on the lines it claims."""

import os
import pathlib

import pytest
from nc_gcode_interpreter import nc_to_dataframe
from polars.testing import assert_frame_equal


def run_both(program, **kwargs):
    previous = os.environ.get("NC_STAGE1")
    try:
        os.environ["NC_STAGE1"] = "0"
        full_df, full_state = nc_to_dataframe(program, **kwargs)
        os.environ["NC_STAGE1"] = "1"
        fast_df, fast_state = nc_to_dataframe(program, **kwargs)
    finally:
        if previous is None:
            os.environ.pop("NC_STAGE1", None)
        else:
            os.environ["NC_STAGE1"] = previous
    assert_frame_equal(full_df, fast_df, check_column_order=False)
    assert full_state["symbol_table"] == fast_state["symbol_table"]
    assert full_state["axes"] == fast_state["axes"]
    assert full_state["translation"] == fast_state["translation"]
    return fast_df


@pytest.mark.parametrize(
    "program",
    [
        # plain flood shapes
        "X1 Y2 Z3\nX4 Y5 Z6",
        "N10 X1.5 Y-2. Z0.001\nN20 X2 F2400 S8000 D1",
        "G1 X1 Y1\nG0 X0\nG54\nG17 G94 G90",
        "BSPLINE\nX1 Y1 PW=1.5\nX2 Y2 PW=0.5\nG1",
        "X1 A=0.0 B=-1.5 C=90. ELX=3087.022334",
        # comments, blanks, N-only lines
        "; header\nX1 ; move\n\nN100\nX2",
        # M codes incl. end-of-program mid-file
        "X1 M3\nX2 M170\nM30\nX999",
        "X1 M101 M102 M2 M3 M4",
        # jumps, labels, block numbers (line-scoped control)
        "GOTOF SKIP\nX999\nSKIP: X2",
        "N40 R1=30 R2=10 R4=3\nN41 LA1: X=R1\nN42 R1=R1+10 R4=R4-1\nN43 IF R4>0 GOTOB LA1\nN44 M30",
        "N10 GOTOF 40\nN20 X999\nN40 X4\nN50 GOTO N70\nN60 X999\nN70 X7",
        "GOTOC NOWHERE\nX2",
        "CASE(5) OF 7 GOTOF SEVEN DEFAULT GOTOF OTHER\nSEVEN: X7\nGOTOF ENDE\nOTHER: X0\nENDE: M30",
        "GOTOS\nX1",
        # lines the decoder must reject to pest, mixed with claimed ones
        "DEF REAL DEPTH=2.5\nX=DEPTH\nTRANS X10\nX1\nTRANS\nX2",
        "R1=1+2\nX=R1*3\nY=SIN(30)\nZ=IC(5)",
        "T=\"drill\"\nX1\nT5",
        "x100\nX100",  # lowercase bare word is a call, uppercase an axis move
        "N10 lowercase=5\nX=lowercase",
        # duplicate block numbers with a directional jump
        "N10 X1\nGOTOF N10\nN10 X2",
    ],
    ids=lambda p: p.splitlines()[0][:30],
)
def test_fast_path_matches_full_parse(program):
    run_both(program)


def test_fast_path_matches_with_extra_axes_and_index_map(tmp_path):
    program = "N10 X1 ELX=100\nFL[E]=10\nN20 X2 ELX=200 E5"
    run_both(program, extra_axes=["ELX"], axis_index_map={"E": 4})


def test_structured_programs_take_the_full_path():
    """Programs with IF/WHILE bodies bypass the fast path entirely and must
    still work (this exercises the shape gate)."""
    df, _state = nc_to_dataframe("R1=1\nIF R1==1\nX5\nENDIF\nX6")
    assert df["X"].drop_nulls().to_list() == [5.0, 6.0]


def test_grammar_heavy_program_declines_the_fast_path():
    """When most lines need the per-line grammar, the newline padding that
    keeps pest positions correct would cost O(n^2) memory; the line driver
    must decline and the whole-file parse take over, transparently."""
    lines = 5000  # padding would be ~12.5 MB, over the 4 MB budget
    program = "\n".join("X=SIN(30)" for _ in range(lines))
    df, _state = nc_to_dataframe(program)
    assert df.height == lines
    assert abs(df["X"][0] - 0.5) < 1e-12


MILL_SIM = pathlib.Path(
    # Override with NC_TEST_CORPUS to run the corpus tests elsewhere; they
    # are skipped when the directory does not exist.
    os.environ.get("NC_TEST_CORPUS", "/Users/thijsdamsma/projects/mill-sim/test-case-1")
)


@pytest.mark.skipif(not MILL_SIM.exists(), reason="mill-sim corpus not available")
@pytest.mark.parametrize(
    "mpf", sorted(MILL_SIM.glob("*.mpf")), ids=lambda p: p.name[:40]
)
def test_fast_path_matches_on_real_corpus(mpf):
    run_both(
        mpf.read_text(errors="replace"),
        extra_axes=["ELX"],
        allow_undefined_variables=True,
    )
