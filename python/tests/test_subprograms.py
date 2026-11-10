"""Subprogram execution, M-function replacement and external variables (#48).

The cycles here are synthetic: they have the shapes of machine cycles (a
parameterised move with a position guard, a wait on an input, a tool change on
M6) and no relation to any real machine's files.
"""

import polars as pl
import pytest
from nc_gcode_interpreter import NcError, nc_to_batches, nc_to_dataframe

GUARD = """PROC AT_POSE(VAR BOOL RESULT, VAR REAL POSE[2], REAL TOLERANCE)
RESULT = FALSE
IF (ABS($AA_IM[X] - POSE[0]) < TOLERANCE) AND (ABS($AA_IM[Y] - POSE[1]) < TOLERANCE)
RESULT = TRUE
ENDIF
RET
"""

GOTO_POSE = """PROC GOTO_POSE(INT TARGET) SAVE
DEF BOOL AT_START
AT_POSE(AT_START, HOME, 0.5)
IF NOT AT_START
SETAL(65000)
GOTOF DONE
ENDIF
TRAFOOF
IF TARGET == 1
G1 X=PARK[0] Y=PARK[1] F=PARK_FEED
ENDIF
DONE:
M17
"""

GUD = "DEF REAL HOME[2]=SET(0, 0)\nDEF REAL PARK[2]=SET(100, 50)\nPARK_FEED=3000\n"


class Machine:
    """Answers position and I/O variables; records what it is asked."""

    names = ("$AA_IM", "$A_IN", "$A_OUT")

    def __init__(self, position: dict[str, float], ready_after: int = 0):
        self.position = position
        self.ready_after = ready_after
        self.outputs: dict[int, float] = {}
        self.rows_seen = 0
        self.log: list[tuple] = []

    def read(self, name, indices):
        self.log.append(("read", name, indices, self.rows_seen))
        if name == "$AA_IM":
            return self.position[indices[0]]
        self.ready_after -= 1
        return 1.0 if self.ready_after < 0 else 0.0

    def write(self, name, indices, value):
        self.log.append(("write", name, indices, value, self.rows_seen))
        self.outputs[indices[0]] = value


def test_pose_cycle_with_position_guard():
    machine = Machine({"X": 0.0, "Y": 0.2})
    df, _ = nc_to_dataframe(
        "G1 X0 Y0\nGOTO_POSE(1)\nG1 X5\n",
        initial_state=GUD,
        subprograms={"GOTO_POSE": GOTO_POSE, "AT_POSE": GUARD},
        external_variables=machine,
        include_line_numbers=True,
    )
    moves = df.filter(
        pl.col("subprogram") == "GOTO_POSE", pl.col("subprogram_line") == 10
    )
    assert moves.select("X", "Y", "F", "line_no").row(0) == (100.0, 50.0, 3000.0, 2)
    assert "TRAFOOF" in df["non_returning_function_call"].to_list()
    assert df["X"].to_list()[-1] == 5.0
    # The guard asked for the actual position by axis name.
    assert [entry[:3] for entry in machine.log] == [
        ("read", "$AA_IM", ("X",)),
        ("read", "$AA_IM", ("Y",)),
    ]


def test_guard_failure_takes_the_abort_branch():
    machine = Machine({"X": 30.0, "Y": 0.0})
    df, _ = nc_to_dataframe(
        "G1 X0 Y0\nGOTO_POSE(1)\n",
        initial_state=GUD,
        subprograms={"GOTO_POSE": GOTO_POSE, "AT_POSE": GUARD},
        external_variables=machine,
    )
    calls = df["non_returning_function_call"].to_list()
    assert "SETAL(65000)" in calls
    assert "TRAFOOF" not in calls
    assert 100.0 not in df["X"].to_list()


def test_provider_is_asked_after_the_preceding_rows_were_yielded():
    wait = "PROC CLAMP\n$A_OUT[3] = 1\nWHILE $A_IN[5] == 0\nG4 F0.5\nENDWHILE\nG1 Z10\n"
    machine = Machine({}, ready_after=2)
    heights = []
    for batch in nc_to_batches(
        "G1 X0 Z0\nCLAMP\nG1 X1\n",
        subprograms={"clamp": wait},
        external_variables=machine,
    ):
        machine.rows_seen += batch.height
        heights.append(batch.height)
    # Two rows precede the write; each failed poll adds one dwell row.
    assert machine.log == [
        ("write", "$A_OUT", (3,), 1.0, 2),
        ("read", "$A_IN", (5,), 2),
        ("read", "$A_IN", (5,), 3),
        ("read", "$A_IN", (5,), 4),
    ]
    assert machine.outputs == {3: 1.0}
    assert sum(heights) == 6


def test_m_function_runs_the_tool_change_cycle_also_from_inside_a_cycle():
    tool_change = "PROC TC\nG1 Z200\nM6\nG1 Z=T\n"
    attach = 'PROC ATTACH(STRING[16] KIND)\nT0 M6\nMSG("loading " << KIND)\n'
    df, _ = nc_to_dataframe(
        'G1 X0 Z0\nT2 M6\nATTACH("SPINDLE")\n',
        subprograms={"TC": tool_change, "ATTACH": attach},
        m_function_subprograms={6: "TC"},
    )
    assert df["subprogram"].drop_nulls().unique().sort().to_list() == [
        "ATTACH",
        "ATTACH/TC",
        "TC",
    ]
    assert df.filter(pl.col("subprogram") == "TC")["Z"].to_list() == [200.0, 200.0, 2.0]
    assert df.filter(pl.col("subprogram") == "ATTACH/TC")["Z"].to_list() == [
        200.0,
        200.0,
        0.0,
    ]


def test_unresolved_calls_and_plain_m_codes_are_unchanged():
    program = "G1 X0\nOTHER(1)\nM6\nG1 X1\n"
    plain, _ = nc_to_dataframe(program)
    with_subs, _ = nc_to_dataframe(program, subprograms={"UNUSED": "G1 X9\n"})
    assert with_subs.equals(plain)
    assert "subprogram" not in plain.columns


def test_locals_do_not_leak_between_levels():
    sub = "PROC BUMP(VAR REAL TOTAL)\nDEF REAL STEP=5\nTOTAL=TOTAL+STEP\n"
    df, state = nc_to_dataframe(
        "DEF REAL STEP=1, SUM=10\nBUMP(SUM)\nG1 X=SUM Y=STEP\n",
        subprograms={"BUMP": sub},
    )
    assert df.select("X", "Y").row(-1) == (15.0, 1.0)
    assert state["symbol_table"]["STEP"] == 1.0
    with pytest.raises(NcError, match="SECRET"):
        nc_to_dataframe(
            "DEF REAL SECRET=1\nPEEK\n", subprograms={"PEEK": "G1 X=SECRET\n"}
        )


def test_errors_name_the_call_and_the_subprogram_line():
    with pytest.raises(NcError) as raised:
        nc_to_dataframe(
            "G1 X0\nBROKEN\n", subprograms={"BROKEN": "G1 X1\nG1 X=UNDEFINED_NAME\n"}
        )
    message = str(raised.value)
    assert raised.value.line == 2
    assert (
        "subprogram BROKEN" in message
        and "UNDEFINED_NAME" in message
        and "line 2" in message
    )
    with pytest.raises(NcError, match="2 arguments passed, the PROC header declares 1"):
        nc_to_dataframe("ONE(1, 2)\n", subprograms={"ONE": "PROC ONE(INT A)\n"})
    with pytest.raises(NcError, match="nested more than"):
        nc_to_dataframe("AGAIN\n", subprograms={"AGAIN": "AGAIN\n"})
    with pytest.raises(NcError, match="not among the supplied subprograms"):
        nc_to_dataframe("M6\n", m_function_subprograms={6: "MISSING"})


def test_provider_exception_propagates_unchanged():
    class Stopped(Exception):
        pass

    class NoPlc:
        names = ("$A_DBB",)

        def read(self, name, indices):
            raise Stopped(f"{name}{list(indices)} has no model")

    with pytest.raises(Stopped, match=r"\$A_DBB\[4\] has no model"):
        list(
            nc_to_batches(
                "G1 X0\nIF $A_DBB[4] == 1\nG1 X1\nENDIF\n", external_variables=NoPlc()
            )
        )
    # A write to a provider without `write` is an error, not a dropped value.
    with pytest.raises(AttributeError):
        nc_to_dataframe("$A_DBB[4] = 1\n", external_variables=NoPlc())


def test_header_declaration_and_return_are_not_calls():
    df, _ = nc_to_dataframe("PROC MAIN SAVE\nEXTERN HELPER(INT)\nG1 X1\nRET\nG1 X2\n")
    assert df["X"].to_list() == [1.0]
    assert "non_returning_function_call" not in df.columns
