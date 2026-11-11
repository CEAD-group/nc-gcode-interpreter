"""definitions, variables and start: what the control holds before the program."""

import polars as pl
import pytest
from nc_gcode_interpreter import NcError, nc_to_batches, nc_to_dataframe, nc_to_rows

MGUD = "%_N_MGUD_DEF\n$PATH=/_N_DEF_DIR\nDEF NCK REAL FIRST = 1.5\nM30\n"
UGUD = (
    "; user data\nDEF CHAN INT COUNT\nDEF CHAN STRING[8] TAG\nCOUNT = 2\nM17\nG1 X99\n"
)


def test_two_definition_files_ending_in_m30_both_take_effect():
    df, state = nc_to_dataframe("X = FIRST + COUNT", definitions=[MGUD, UGUD])
    assert df["X"].to_list() == [3.5]
    assert state["symbol_table"]["COUNT"] == 2


def test_definition_files_may_be_paths(tmp_path):
    path = tmp_path / "UGUD.DEF"
    path.write_text(UGUD)
    df, _ = nc_to_dataframe("X = COUNT", definitions=[path])
    assert df["X"].to_list() == [2.0]


def test_a_single_string_is_not_a_list_of_files():
    with pytest.raises(TypeError, match="list of definition files"):
        nc_to_dataframe("X1", definitions=MGUD)


@pytest.mark.parametrize(
    "line",
    ["G1 X10", "X=5", "TRANS X1", "LABEL: R1=1", "GOTOF LABEL", "$P_UIFR[1]=0"],
)
def test_anything_but_definitions_in_a_definition_file_is_an_error(line):
    with pytest.raises(NcError) as info:
        nc_to_dataframe("X1", definitions=[MGUD, f"DEF REAL OK\n{line}\n"])
    assert info.value.definition_file == 1
    assert info.value.line == 2
    assert "definitions[1]" in str(info.value)


def test_an_error_in_the_program_has_no_definition_file():
    with pytest.raises(NcError) as info:
        nc_to_dataframe("X = UNKNOWN", definitions=[MGUD])
    assert info.value.definition_file is None
    assert info.value.line == 1


def test_variables_are_applied_after_the_definitions():
    df, state = nc_to_dataframe(
        "X = COUNT Y = FIRST",
        definitions=[MGUD, UGUD],
        variables={"COUNT": 7, "FIRST": 0.25, "TAG": "abc", "R3": 1.0},
    )
    assert df["X"].to_list() == [7.0]
    assert df["Y"].to_list() == [0.25]
    assert state["string_table"]["TAG"] == "abc"
    assert state["symbol_table"]["R3"] == 1.0


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("COUNT", 2.5),
        ("COUNT", "two"),
        ("FIRST", "x"),
        ("TAG", 1.0),
        ("TAG", "longer than eight"),
        ("NOT_DEFINED", 1.0),
        ("X", 1.0),
    ],
)
def test_a_value_that_does_not_fit_is_an_error(name, value):
    with pytest.raises(NcError) as info:
        nc_to_dataframe("X1", definitions=[MGUD, UGUD], variables={name: value})
    assert info.value.kind == "invalid_input"
    assert f"variables['{name}']" in str(info.value)


def test_undefined_variables_follow_allow_undefined_variables():
    _, state = nc_to_dataframe(
        "X1", variables={"LOOSE": 4, "LOOSE_TEXT": "a"}, allow_undefined_variables=True
    )
    assert state["symbol_table"]["LOOSE"] == 4.0
    assert state["string_table"]["LOOSE_TEXT"] == "a"


def test_a_value_of_the_wrong_python_type_raises_at_the_call():
    with pytest.raises(ValueError, match="must be a float, int, bool or str"):
        nc_to_dataframe("X1", variables={"R1": [1, 2]})


def test_a_def_in_the_program_reinitialises_a_passed_value():
    df, _ = nc_to_dataframe(
        "X = COUNT\nDEF INT COUNT\nX = COUNT",
        definitions=[UGUD],
        variables={"COUNT": 5},
    )
    assert df["X"].to_list() == [5.0, 0.0]


START = {
    "axes": {"X": 10.0, "Y": 20.0},
    "g_groups": {"gg01_motion": "G1", "gg06_plane_select": "G17"},
    "feed": 1500.0,
}


def test_start_seeds_the_first_row_g_groups_and_feed():
    df, state = nc_to_dataframe("X15\nG0 X0", start=START)
    assert df["gg01_motion"].to_list() == ["G1", "G0"]
    assert df["gg06_plane_select"].to_list() == ["G17", "G17"]
    assert df["F"].to_list() == [1500.0, 1500.0]
    # Positions seed the internal state only: no Y column on the first row.
    assert "Y" not in df.columns
    assert state["axes"]["Y"] == 20.0
    assert state["g_groups"]["gg01_motion"] == "G0"
    assert state["feed"] == 1500.0


def test_start_seeds_every_entry_point():
    rows = list(nc_to_rows("X15", start=START))
    assert rows[0][1]["gg01_motion"] == "G1"
    assert rows[0][1]["F"] == 1500.0
    batches = list(nc_to_batches("X15", start=START, batch_size=1))
    assert pl.concat(batches)["F"].to_list() == [1500.0]


@pytest.mark.parametrize(
    "start",
    [
        {"g_groups": {"gg01_motion": "G90"}},
        {"g_groups": {"gg01_motion": "G9999"}},
        {"g_groups": {"gg02_wait": "G4"}},
        {"g_groups": {"motion": "G1"}},
        {"axes": {"Q": 1.0}},
        {"rotation": [[2, 0, 0], [0, 1, 0], [0, 0, 1]]},
        {"rotation": [[-1, 0, 0], [0, 1, 0], [0, 0, 1]]},
    ],
)
def test_invalid_start_values_are_errors(start):
    with pytest.raises(NcError) as info:
        nc_to_dataframe("X1", start=start)
    assert info.value.kind == "invalid_input"


def test_unknown_start_key_raises_at_the_call():
    with pytest.raises(ValueError, match="unknown key 'positions'"):
        nc_to_dataframe("X1", start={"positions": {"X": 1.0}})


def test_a_name_in_variables_and_start_is_an_error():
    with pytest.raises(NcError, match="give it in one place"):
        nc_to_dataframe(
            "X1",
            definitions=[UGUD],
            variables={"COUNT": 1},
            start={"symbol_table": {"COUNT": 2.0}},
        )


def test_returned_state_passed_back_as_start_gives_the_same_first_row():
    program = 'DEF STRING[4] NAME = "ab"\nT3\nTRAORI\nG1 G17 G90 X1 Y2 F300\nG2 X3 Y4 I1 J0\nTRANS X5\n'
    _, state = nc_to_dataframe(program, definitions=[MGUD])
    df, again = nc_to_dataframe("X7", definitions=[MGUD], start=state)
    first = df.row(0, named=True)
    for group, code in state["g_groups"].items():
        assert first[group] == code
    assert first["F"] == state["feed"]
    assert (state["tool"], state["transformation"]) == ("3", "TRAORI")
    assert (first["T"], first["transformation"]) == ("3", "TRAORI")
    # The start frame (TRANS X5) applies to the first move.
    assert first["X"] == 12.0
    assert again["g_groups"] == state["g_groups"]
    assert again["string_table"] == state["string_table"]
    assert again["translation"] == state["translation"]
    assert (again["tool"], again["transformation"]) == ("3", "TRAORI")


def test_tool_and_transformation_seed_the_first_row():
    df, state = nc_to_dataframe(
        "G0 X1\nT2 X2", start={"tool": 7, "transformation": "TRAFOOF"}
    )
    assert df["T"].to_list() == ["7", "2"]
    assert df["transformation"].to_list() == ["TRAFOOF", "TRAFOOF"]
    assert state["tool"] == "2"


@pytest.mark.parametrize("start", [{"tool": ""}, {"transformation": "G1"}])
def test_invalid_tool_or_transformation_is_an_error(start):
    with pytest.raises(NcError) as info:
        nc_to_dataframe("G0 X1", start=start)
    assert info.value.kind == "invalid_input"


def test_chandata_1_in_a_definition_file_is_ignored():
    df, _ = nc_to_dataframe(
        "X = COUNT", definitions=["CHANDATA(1)\nDEF CHAN INT COUNT = 4\nM17\n"]
    )
    assert df["X"].to_list() == [4.0]


def test_another_channel_in_a_definition_file_is_an_error():
    with pytest.raises(NcError, match="CHANDATA\\(2\\)") as info:
        nc_to_dataframe("X1", definitions=["CHANDATA(2)\nDEF CHAN INT COUNT\n"])
    assert info.value.definition_file == 0
    assert info.value.line == 1


def test_assigning_an_undeclared_name_in_a_definition_file_is_an_error():
    with pytest.raises(NcError) as info:
        nc_to_dataframe("X1", definitions=["DEF CHAN INT COUNT\nCOUNTT = 2\n"])
    assert info.value.definition_file == 0
    assert info.value.line == 2
    assert "COUNTT" in str(info.value)


def test_r_parameters_and_allow_undefined_variables_permit_assignment():
    _, state = nc_to_dataframe("X1", definitions=["R7 = 2\n"])
    assert state["symbol_table"]["R7"] == 2.0
    _, state = nc_to_dataframe(
        "X1", definitions=["LOOSE = 3\n"], allow_undefined_variables=True
    )
    assert state["symbol_table"]["LOOSE"] == 3.0


def test_feed_group_seeds_the_first_row_and_round_trips():
    start = {"fgroup": "X,A", "fgref": {"A": 57.3}, "feed": 100.0}
    df, state = nc_to_dataframe("G1 A10\nFGREF[B]=2 X1\n", start=start)
    assert df["FGROUP"].to_list() == ["X,A", "X,A"]
    assert df["FGREF[A]"].to_list() == [57.3, 57.3]
    assert (state["fgroup"], state["fgref"]) == ("X,A", {"A": 57.3, "B": 2.0})
    again, _ = nc_to_dataframe("A20\n", start=state)
    assert again.row(0, named=True)["FGROUP"] == "X,A"
    assert again.row(0, named=True)["FGREF[B]"] == 2.0


@pytest.mark.parametrize(
    "start", [{"fgroup": "X,F"}, {"fgref": {"Q": 1.0}}, {"fgref": {"A": -1.0}}]
)
def test_invalid_feed_group_is_an_error(start):
    with pytest.raises(NcError) as info:
        nc_to_dataframe("G0 X1", start=start)
    assert info.value.kind == "invalid_input"
