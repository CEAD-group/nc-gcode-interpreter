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
        nc_to_dataframe("ONE(1, 2)\n", subprograms={"ONE": "PROC ONE(INT COUNT)\n"})
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


class Station:
    """Serves a STRING state, a frame variable and machine-axis positions."""

    names = ("STATION_STATE", "$P_UIFR", "$AA_IW")

    def __init__(self, state: str):
        self.state = state
        self.writes: list[tuple] = []
        self.reads: list[tuple] = []

    def read(self, name, indices):
        self.reads.append((name, indices))
        if name == "STATION_STATE":
            return self.state
        return 12.5

    def write(self, name, indices, value):
        self.writes.append((name, indices, value))


def test_definition_file_attributes_are_accepted_and_ignored():
    gud = (
        "DEF CHAN SYNRW REAL REACH[4]\n"
        "DEF NCK APW 4 INT COUNT\n"
        "DEF CHAN REAL APWB 7 LLI 0 ULI 100.5 PHU 4 LIMITED = 5\n"
        "DEF CHAN CHAR CODE\n"
        "REDEF COUNT APW 7\n"
        "REDEF LIMITED PRLOC\n"
    )
    _, state = nc_to_dataframe("COUNT = COUNT + 2\nREACH[3] = LIMITED\n", initial_state=gud)
    assert state["symbol_table"]["COUNT"] == 2
    assert state["symbol_table"]["REACH[3]"] == 5
    # A name that only starts like an attribute stays a variable.
    _, state = nc_to_dataframe("DEF REAL CHANNEL = 3\n")
    assert state["symbol_table"]["CHANNEL"] == 3


def test_redef_with_a_value_fails_loudly():
    with pytest.raises(NcError):
        nc_to_dataframe("DEF REAL A\nREDEF A INIPO(5)\n")


def test_string_comparison_in_conditions():
    program = (
        'DEF STRING[20] KIND = "MILL"\n'
        'IF (KIND == "MILL")\nX1\nENDIF\n'
        'IF KIND <> "MILL"\nX2\nENDIF\n'
        'IF ("MILL" == KIND) AND (KIND <> "mill")\nX3\nENDIF\n'
        'R1 = (KIND == "DRILL")\n'
    )
    df, state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [1.0, 3.0]
    assert state["symbol_table"]["R1"] == 0


@pytest.mark.parametrize(
    ("line", "message"),
    [
        ('IF KIND < "MILL" GOTOF END', "== and <>"),
        ("IF KIND == 1 GOTOF END", "compared with a number"),
        ('R1 = 1 + "A"', "cannot be used as a number"),
    ],
)
def test_string_misuse_in_expressions_is_an_error(line, message):
    with pytest.raises(NcError, match=message):
        nc_to_dataframe(f'DEF STRING[20] KIND = "MILL"\n{line}\nEND:\n')


def test_served_string_variable_is_read_as_text_in_a_comparison():
    program = 'IF (STATION_STATE == "READY")\nX1\nELSE\nX2\nENDIF\n'
    station = Station("READY")
    df, _ = nc_to_dataframe(program, external_variables=station)
    assert df["X"].drop_nulls().to_list() == [1.0]
    assert station.reads == [("STATION_STATE", ())]
    # A number where the program needs text is the provider's error.
    station.read = lambda name, indices: 1.0
    with pytest.raises(TypeError):
        nc_to_dataframe(program, external_variables=station)


def test_frame_assignment_reaches_the_provider():
    station = Station("READY")
    program = "R1 = 90\n$P_UIFR[7] = CTRANS(X, 10, Y, R1 / 2) : CROT(Z, R1)\nX = $AA_IW[AX1]\n"
    df, _ = nc_to_dataframe(program, external_variables=station)
    assert station.writes == [("$P_UIFR", (7,), [("CTRANS", ("X", 10.0, "Y", 45.0)), ("CROT", ("Z", 90.0))])]
    assert station.reads == [("$AA_IW", ("AX1",))]
    assert df["X"].drop_nulls().to_list() == [12.5]
    with pytest.raises(NcError, match="external_variables"):
        nc_to_dataframe("$P_UIFR[7] = CTRANS(X, 10)\n")


def test_transformation_column_is_modal():
    program = "X1\nTRAORI(1)\nX2\nCALLED(1)\nX3\nTRAFOOF\nX4\n"
    df, _ = nc_to_dataframe(program)
    assert df["transformation"].to_list() == [None, "TRAORI(1)", "TRAORI(1)", "TRAORI(1)", "TRAORI(1)", "TRAFOOF", "TRAFOOF"]
    assert df["non_returning_function_call"].drop_nulls().to_list() == ["TRAORI(1)", "CALLED(1)", "TRAFOOF"]
    assert "transformation" not in nc_to_dataframe("X1\nCALLED(1)\n")[0].columns


def test_stop_on_setal_reports_the_alarm_and_the_call_path():
    cycles = {"OUTER": "PROC OUTER\nINNER\nM17\n", "INNER": 'PROC INNER\nX1\nSETAL(65010, "no tool")\nX2\nM17\n'}
    df, _ = nc_to_dataframe("OUTER\n", subprograms=cycles)
    assert df["X"].drop_nulls().unique(maintain_order=True).to_list() == [1.0, 2.0]
    with pytest.raises(NcError) as error:
        nc_to_dataframe("OUTER\n", subprograms=cycles, stop_on_setal=True)
    text = str(error.value)
    assert 'SETAL(65010, "no tool")' in text
    assert "OUTER" in text and "INNER" in text


def test_required_subprogram_that_is_not_supplied_is_an_error():
    cycles = {"OUTER": "PROC OUTER\nATTACH(2)\nM17\n"}
    df, _ = nc_to_dataframe("OUTER\n", subprograms=cycles)
    assert "ATTACH(2)" in df["non_returning_function_call"].to_list()
    with pytest.raises(NcError, match="ATTACH is a required subprogram") as error:
        nc_to_dataframe("OUTER\n", subprograms=cycles, required_subprograms=["attach"])
    assert "OUTER" in str(error.value)


def test_array_assignment_with_set_and_rep_outside_a_definition():
    program = (
        "DEF REAL POSE[8]\nDEF REAL GRID[2,3]\n"
        "POSE[0] = SET( 0.000, -30.000, -70.000, 0.000, 19.656, 0.000, 3000, 25000) ; taught\n"
        "POSE[2] = SET(7, , 9)\n"
        "GRID[0,2] = SET(1, 2, 3)\n"
        "POSE[6] = REP(5, 2)\n"
    )
    _, state = nc_to_dataframe(program)
    table = state["symbol_table"]
    assert [table[f"POSE[{i}]"] for i in range(8)] == [0, -30, 7, 0, 9, 0, 5, 5]
    assert (table["GRID[0,2]"], table["GRID[1,0]"], table["GRID[1,1]"]) == (1, 2, 3)
    with pytest.raises(NcError, match="not a defined array"):
        nc_to_dataframe("UNKNOWN[0,0] = SET(1, 2)\n")


class Recorder:
    """Serves the given names with one value and records every call."""

    def __init__(self, names, value=0.0):
        self.names = names
        self.value = value
        self.calls = []

    def read(self, name, indices):
        self.calls.append(("read", name, indices))
        return self.value

    def write(self, name, indices, value):
        self.calls.append(("write", name, indices, value))


@pytest.mark.parametrize(
    "definition, name",
    [
        ("DEF REAL APR1=3", "APR1"),
        ("DEF REAL CHAN=5", "CHAN"),
        ("DEF INT NCK=2", "NCK"),
        ("DEF REAL PHU2=1", "PHU2"),
        ("DEF REAL LLI5=3, ULI2=4", "LLI5"),
        ("DEF NCK REAL SYNR=1", "SYNR"),
    ],
)
def test_a_variable_may_be_spelled_like_a_definition_attribute(definition, name):
    df, state = nc_to_dataframe(f"{definition}\nG1 X={name}\n")
    assert "non_returning_function_call" not in df.columns
    assert df["X"].to_list() == [state["symbol_table"][name]]


def test_for_counter_and_local_names_cannot_be_external_variables():
    served = Recorder(["CNT"])
    with pytest.raises(NcError, match="FOR counter 'CNT' is an external variable"):
        nc_to_dataframe("FOR CNT=1 TO 2\nG1 X=CNT\nENDFOR\n", external_variables=served)
    assert served.calls == []
    with pytest.raises(NcError, match="parameter CNT of the PROC header has the name of an external variable"):
        nc_to_dataframe("FOO(3)\n", subprograms={"FOO": "PROC FOO(REAL CNT)\nG1 X=CNT\n"}, external_variables=served)
    with pytest.raises(NcError, match="'CNT' is an external variable; the program cannot define"):
        nc_to_dataframe("FOO\n", subprograms={"FOO": "DEF REAL CNT=1\nG1 X=CNT\n"}, external_variables=served)


def test_a_string_cannot_be_assigned_to_an_external_variable():
    served = Recorder(["MYS"])
    with pytest.raises(NcError, match="'MYS' is an external variable; a string cannot be assigned"):
        nc_to_dataframe('MYS="abc"\n', external_variables=served)


def test_repeat_count_after_a_call_is_an_error():
    for call in ("FOO P3", "FOO P=3", "FOO(1) P2", "FOO P=R1"):
        with pytest.raises(NcError, match=r"a repeat count \(P=?\w+\) after the call of FOO is not supported"):
            nc_to_dataframe(f"R1=2\n{call}\nG1 X1\n", subprograms={"FOO": "PROC FOO(REAL Q=0)\nG1 Y1\nM17\n"})


def test_save_restores_the_modal_g_functions_of_the_caller():
    body = "PROC FOO SAVE\nG91 G18\nG0 X5\n"
    df, _ = nc_to_dataframe("G90 G17 G1 X0 F100\nFOO\nX1\n", subprograms={"FOO": body})
    after = df.filter(pl.col("subprogram").is_null())[-1]
    assert after["gg14_wp_measure_mode"].item() == "G90"
    assert after["gg06_plane_select"].item() == "G17"
    assert after["gg01_motion"].item() == "G1"
    inside = df.filter(pl.col("subprogram") == "FOO")
    assert inside["gg14_wp_measure_mode"].to_list()[-1] == "G91"
    # Without SAVE the subprogram's G functions stay active.
    df, _ = nc_to_dataframe("G90 G17 G1 X0 F100\nFOO\nX1\n", subprograms={"FOO": body.replace(" SAVE", "")})
    assert df["gg14_wp_measure_mode"].to_list()[-1] == "G91"


def test_save_warns_when_the_value_before_the_call_is_unknown(capfd):
    nc_to_dataframe("G1 X0 F100\nFOO\nX1\n", subprograms={"FOO": "PROC FOO SAVE\nG18\nG1 X5\n"})
    assert "the value of gg06_plane_select before the call is not known" in capfd.readouterr().err


def test_var_parameter_keeps_a_direct_write_to_the_callers_variable():
    program = "R1=1\nR2=1\nF(R1, R2)\nG1 X=R1 Y=R2\n"
    df, _ = nc_to_dataframe(program, subprograms={"F": "PROC F(VAR REAL PA, VAR REAL PB)\nR1=5\nPB=7\nM17\n"})
    assert (df["X"].to_list()[-1], df["Y"].to_list()[-1]) == (5.0, 7.0)
    both = "PROC F(VAR REAL PA)\nR1=5\nPA=PA+1\nM17\n"
    with pytest.raises(NcError, match="wrote R1 both directly and through its VAR parameter PA"):
        nc_to_dataframe("R1=1\nF(R1)\n", subprograms={"F": both})


def test_ret_with_a_return_target_is_an_error():
    with pytest.raises(NcError, match="RET with arguments"):
        nc_to_dataframe("FOO\nG1 X1\n", subprograms={"FOO": "G1 Y1\nRET(\"LBL\")\n"})


def test_parameter_with_an_axis_name_is_an_error():
    with pytest.raises(NcError, match="parameter A of the PROC header has the name of an axis"):
        nc_to_dataframe("FOO(5)\n", subprograms={"FOO": "PROC FOO(REAL A)\nG1 X=A\n"})


def test_flattened_rows_keep_the_subprogram_columns():
    body = "G2 X10 Y0 I5 J0\nBSPLINE X20 Y5\nX30 Y0\nX40 Y5\nG1 X50\n"
    df, _ = nc_to_dataframe("G1 X0 Y0 F100\nFOO\nG1 X0\n", subprograms={"FOO": body}, flatten_tolerance=0.5)
    inside = df.filter(pl.col("subprogram_line").is_not_null())
    assert inside.height > 6
    assert inside["subprogram"].unique().to_list() == ["FOO"]
    assert sorted(inside["subprogram_line"].unique().to_list()) == [1, 2, 3, 4, 5]
    assert df.filter(pl.col("subprogram").is_null()).height == 3


@pytest.mark.parametrize(
    "line, message",
    [
        ("QA[1]=SET(1,2,3,4,5)", "more values than 'QA' has elements"),
        ("QA[2]=REP(1,2)", "more values than 'QA' has elements"),
        ("QU[0]=SET(1,2)", "'QU' is not a defined array"),
        ("QB[1]=SET(1,2)", "'QB' is not a defined array of this dimension"),
    ],
)
def test_array_assignment_stays_inside_the_declared_array(line, message):
    with pytest.raises(NcError, match=message):
        nc_to_dataframe(f"DEF REAL QA[3]\nDEF REAL QB[2,2]\n{line}\n")


def test_subprogram_names_that_differ_only_in_case_are_refused():
    with pytest.raises(ValueError, match="more than one entry for FOO"):
        nc_to_dataframe("FOO\n", subprograms={"foo": "G1 X1\n", "FOO": "G1 X2\n"})
