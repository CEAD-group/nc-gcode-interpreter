"""nc_to_rows: the streaming twin of nc_to_dataframe. Every program must
yield exactly the batch DataFrame's rows, in order, with source line
numbers attached."""

import os
import pathlib

import polars as pl
import pytest
from nc_gcode_interpreter import nc_to_dataframe, nc_to_rows
from polars.testing import assert_frame_equal


def assert_stream_matches_batch(program, **kwargs):
    df, state = nc_to_dataframe(program, **kwargs)
    iterator = nc_to_rows(program, **kwargs)
    rows = list(iterator)

    assert len(rows) == df.height
    streamed = pl.DataFrame(
        [row for _line, row in rows],
        schema={name: df.schema[name] for name in df.columns},
    )
    assert_frame_equal(df, streamed, check_column_order=False)
    assert iterator.state["axes"] == state["axes"]
    assert iterator.state["symbol_table"] == state["symbol_table"]
    return rows


@pytest.mark.parametrize(
    "program",
    [
        "G1 X10\nX20 Y5 ; move\nM30",
        "N10 X1.5 Y-2. Z0.001\nN20 X2 F2400 S8000 M3\n; comment only\nX3",
        "BSPLINE\nX1 Y1 PW=1.5\nX2 Y2\nG1",
        "GOTOF SKIP\nX999\nSKIP: X2",  # streamed through the line driver
        "R1=0\nWHILE R1<3\nX=R1\nR1=R1+1\nENDWHILE\nM30",  # whole-file path
        "DEF REAL D_VAL=2.5\nTRANS X10\nX1\nTRANS\nX2",
        "X1\nM30\nX999",
    ],
    ids=lambda p: p.splitlines()[0][:25],
)
def test_stream_matches_batch(program):
    assert_stream_matches_batch(program)


def test_line_numbers_follow_execution_order():
    program = "\n".join(
        [
            "R1=0",          # line 1
            "WHILE R1<2",    # line 2
            "X=R1",          # line 3
            "R1=R1+1",       # line 4
            "ENDWHILE",      # line 5
            "X9",            # line 6
        ]
    )
    rows = list(nc_to_rows(program))
    lines = [line for line, _row in rows]
    # The loop body executes twice: line 3 repeats (line 4 assigns a plain
    # variable and therefore emits no output row).
    assert lines == [3, 3, 6]


def test_jump_line_numbers_are_source_positions():
    rows = list(nc_to_rows("N10 X1\nGOTOF 40\nN30 X999\nN40 X4"))
    assert [line for line, _row in rows] == [1, 4]


def test_forward_fill_can_be_disabled():
    rows = list(nc_to_rows("G1 X10\nY5", forward_fill=False))
    assert "X" not in rows[1][1]
    assert "gg01_motion" not in rows[1][1]


def test_early_exit_does_not_hang():
    iterator = nc_to_rows("X1\n" * 100_000)
    for _ in range(3):
        next(iterator)
    del iterator  # must abort the worker without blocking


def test_error_surfaces_at_the_offending_row():
    iterator = nc_to_rows("X1\nX2\nX=UNDEFINED_VAR")
    assert next(iterator)[0] == 1
    assert next(iterator)[0] == 2
    with pytest.raises(ValueError, match="UNDEFINED_VAR"):
        next(iterator)


def test_parse_error_raises_before_first_row():
    iterator = nc_to_rows("X1\nIF R1>0\nX2")
    with pytest.raises(ValueError, match="IF is never closed"):
        next(iterator)


def test_state_is_none_until_exhausted():
    iterator = nc_to_rows("X1\nX2")
    assert iterator.state is None
    list(iterator)
    assert iterator.state["axes"] == {"X": 2.0}


def test_initial_state_rows_are_not_streamed():
    rows = list(nc_to_rows("X1", initial_state="Y99\nDEF REAL Q=1"))
    assert len(rows) == 1
    assert rows[0][1]["X"] == 1.0
    assert "Y" not in rows[0][1]  # initial-state moves set state, not rows


def test_include_variables_exposes_assignments():
    program = "\n".join(
        [
            "DEF REAL Q=2.5",  # line 1: definition, no output cells
            "R1=0",            # line 2: variable-only
            "WHILE R1<2",      # line 3
            "X=R1",            # line 4: output row, no variable change
            "R1=R1+1",         # line 5: variable-only, twice
            "ENDWHILE",        # line 6
            "X=Q",             # line 7
        ]
    )
    rows = list(nc_to_rows(program, include_variables=True))
    assert [(line, vars) for line, _row, vars in rows] == [
        (1, {"Q": 2.5}),
        (2, {"R1": 0.0}),
        (4, {}),
        (5, {"R1": 1.0}),
        (4, {}),
        (5, {"R1": 2.0}),
        (7, {}),
    ]
    # Variable-only blocks yield an empty, never forward-filled row dict.
    assert rows[0][1] == {}
    assert rows[3][1] == {}
    assert rows[2][1]["X"] == 0.0
    assert rows[6][1]["X"] == 2.5


def test_include_variables_covers_def_multi_and_for_counter():
    program = "\n".join(
        [
            "DEF REAL QA, QB[2]",           # bare defs initialize to 0
            "DEF REAL QC[3]=SET(1,,3)",     # gap: QC[1] stays untouched
            "FOR R7=1 TO 2",
            "X=R7",
            "ENDFOR",
        ]
    )
    rows = list(nc_to_rows(program, include_variables=True))
    variables = [(line, vars) for line, _row, vars in rows]
    assert variables[0] == (1, {"QA": 0.0, "QB[0]": 0.0, "QB[1]": 0.0, "QB[2]": 0.0})
    assert variables[1] == (2, {"QC[0]": 1.0, "QC[2]": 3.0})
    assert variables[2] == (3, {"R7": 1.0})  # FOR init on the FOR line
    # Counter increments surface on the ENDFOR line, where the control
    # performs them; the X=R7 body rows in between carry no changes.
    assert variables[3] == (4, {})
    assert variables[4] == (5, {"R7": 2.0})
    assert variables[5] == (4, {})
    assert variables[6] == (5, {"R7": 3.0})


def test_accumulated_variables_match_final_state():
    program = "\n".join(
        [
            "DEF REAL Q=1",
            "R1=0",
            "WHILE R1<3",
            "X=R1 Q=Q*2",
            "R1=R1+1",
            "ENDWHILE",
        ]
    )
    iterator = nc_to_rows(program, include_variables=True)
    accumulated = {}
    for _line, _row, variables in iterator:
        accumulated.update(variables)
    symbol_table = dict(iterator.state["symbol_table"])
    for name in ("TRUE", "FALSE"):  # built-ins, never assigned by the program
        symbol_table.pop(name)
    assert accumulated == symbol_table


def test_include_variables_off_keeps_two_tuples_and_hides_variable_rows():
    rows = list(nc_to_rows("R1=5\nX=R1"))
    assert rows == [(2, {"X": 5.0})]


MILL_SIM = pathlib.Path(
    # Override with NC_TEST_CORPUS to run the corpus test elsewhere; it is
    # skipped when the directory does not exist.
    os.environ.get("NC_TEST_CORPUS", "/Users/thijsdamsma/projects/mill-sim/test-case-1")
)


@pytest.mark.skipif(not MILL_SIM.exists(), reason="mill-sim corpus not available")
def test_stream_matches_batch_on_real_program():
    program = (MILL_SIM / "Test Sheffield_ROUGHmpf.mpf").read_text(errors="replace")
    assert_stream_matches_batch(
        program, extra_axes=["ELX"], allow_undefined_variables=True
    )


def test_path_input_matches_string_input(tmp_path):
    """A pathlib.Path/os.PathLike input reads the file in Rust and must be
    byte-for-byte equivalent to passing the same program as a string. A plain
    str stays a program (backwards compatible), never a path."""
    program = "G1 X10 Y20\nX20 Y5\nR1=5\nX=R1\n"
    mpf = tmp_path / "prog.mpf"
    mpf.write_text(program)

    assert list(nc_to_rows(mpf)) == list(nc_to_rows(program))

    df_path, state_path = nc_to_dataframe(mpf)
    df_str, state_str = nc_to_dataframe(program)
    assert_frame_equal(df_path, df_str)
    assert state_path == state_str


def test_missing_path_raises_valueerror():
    with pytest.raises(ValueError):
        list(nc_to_rows(pathlib.Path("/definitely/not/here.mpf")))
    with pytest.raises(ValueError):
        nc_to_dataframe(pathlib.Path("/definitely/not/here.mpf"))
