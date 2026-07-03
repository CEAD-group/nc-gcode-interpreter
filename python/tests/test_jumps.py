"""Program jumps to jump markers: GOTOB, GOTOF, GOTO, GOTOC, GOTOS,
IF ... GOTO, CASE ... OF ... DEFAULT (NC programming manual 4.1.5)."""

import pytest
from nc_gcode_interpreter import nc_to_dataframe


def test_gotof_skips_to_forward_label():
    df, _state = nc_to_dataframe("GOTOF SKIP\nX999\nSKIP: X2")
    assert df["X"].to_list() == [2.0]


def test_gotob_jumps_backward():
    """Manual 4.1.5.2 example 4: a conditional backward jump forms a loop."""
    program = "\n".join(
        [
            "N40 R1=30 R2=10 R4=3",
            "N41 LA1: X=R1",
            "N42 R1=R1+R2 R4=R4-1",
            "N43 IF R4>0 GOTOB LA1",
            "N44 M30",
        ]
    )
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().unique(maintain_order=True).to_list() == [30.0, 40.0, 50.0]


def test_goto_searches_forward_then_backward():
    # BACK lies behind the GOTO, so only the backward leg of the search can
    # find it; the loop then runs until R1 reaches 3.
    program = "R1=0\nBACK: R1=R1+1\nIF R1<3 GOTO BACK\nX=R1"
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [3.0]


def test_jump_to_block_number_with_and_without_n():
    df, _state = nc_to_dataframe("N10 GOTOF 40\nN20 X999\nN40 X4\nN50 GOTO N70\nN60 X999\nN70 X7")
    assert df["X"].drop_nulls().to_list() == [4.0, 4.0, 7.0]


def test_labels_are_case_insensitive():
    df, _state = nc_to_dataframe("GOTOF ende\nX999\nEnde: X1")
    assert df["X"].to_list() == [1.0]


def test_label_after_block_number_executes_rest_of_block():
    df, _state = nc_to_dataframe("N10 GOTOF LAB\nN20 LAB: X5")
    assert df["X"].to_list() == [None, 5.0]


def test_gotoc_missing_destination_continues():
    df, _state = nc_to_dataframe("GOTOC NOWHERE\nX2")
    # The jump-only block emits no output row of its own.
    assert df["X"].to_list() == [2.0]


def test_goto_missing_destination_raises():
    with pytest.raises(ValueError, match="not found"):
        nc_to_dataframe("GOTOF NOWHERE\nX2")


def test_gotof_does_not_search_backward():
    with pytest.raises(ValueError, match="not found"):
        nc_to_dataframe("BEHIND: X1\nGOTOF BEHIND")


def test_jump_into_control_structure_raises():
    program = "R1=0\nGOTOF INSIDE\nIF R1==0\nINSIDE: X1\nENDIF"
    with pytest.raises(ValueError, match="not found"):
        nc_to_dataframe(program)


def test_jump_out_of_if_body():
    program = "\n".join(
        [
            "R1=1",
            "IF R1==1",
            "GOTOF DONE",
            "ENDIF",
            "X999",
            "DONE: X1",
        ]
    )
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [1.0]


def test_loop_left_with_jump():
    """LOOP ... ENDLOOP is an endless loop left via a jump (manual 4.1.7.2)."""
    program = "\n".join(
        [
            "R1=0",
            "LOOP",
            "X=R1",
            "R1=R1+1",
            "IF R1>=3 GOTOF DONE",
            "ENDLOOP",
            "DONE: M30",
        ]
    )
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().unique(maintain_order=True).to_list() == [0.0, 1.0, 2.0]


def test_loop_without_jump_hits_iteration_limit():
    with pytest.raises(ValueError, match="[Ll]oop limit"):
        nc_to_dataframe("LOOP\nX1\nENDLOOP", iteration_limit=10)


def test_backward_jump_cycle_hits_iteration_limit():
    with pytest.raises(ValueError, match="[Ll]oop limit"):
        nc_to_dataframe("AGAIN: X1\nGOTOB AGAIN", iteration_limit=10)


def test_forward_jumps_do_not_count_against_iteration_limit():
    """Forward jumps strictly advance and cannot cycle; a program with more
    forward jumps than the iteration limit must still run to completion."""
    blocks = []
    for i in range(20):
        blocks.append(f"GOTOF SKIP{i}")
        blocks.append(f"SKIP{i}: X{i}")
    df, _state = nc_to_dataframe("\n".join(blocks), iteration_limit=10)
    assert df["X"].to_list()[-1] == 19.0


def test_multiple_conditional_jumps_in_one_block():
    """Several jump statements with conditions may share a block (4.1.5.2)."""
    program = "\n".join(
        [
            "R1=2",
            "IF R1==1 GOTOF ONE IF R1==2 GOTOF TWO",
            "ONE: X1",
            "GOTOF ENDE",
            "TWO: X2",
            "ENDE: M30",
        ]
    )
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [2.0, 2.0]


def test_gotos_continues_without_plc_request():
    """GOTOS restarts the program only on a PLC request (enableGoToStart);
    without a PLC the documented default is to continue with the next block."""
    df, _state = nc_to_dataframe("X1\nGOTOS\nX2")
    # The GOTOS-only block emits no output row of its own.
    assert df["X"].to_list() == [1.0, 2.0]


def test_m30_ends_program():
    """With jumps, M2/M30 need not be the last block (manual 4.1.5.2): blocks
    after an executed end-of-program M code must not run."""
    df, _state = nc_to_dataframe("X1\nM30\nX999")
    assert df["X"].to_list() == [1.0, 1.0]


def test_m2_ends_program_but_finishes_its_block():
    df, _state = nc_to_dataframe("X1\nX2 M2\nX999")
    assert df["X"].to_list() == [1.0, 2.0]


def test_case_of_default():
    program = "\n".join(
        [
            "DEF INT VAR1 = 4",
            "DEF INT VAR2 = 6",
            "DEF INT VAR3 = 3",
            "N30 CASE(VAR1+VAR2-VAR3) OF 7 GOTOF LABEL_1 9 GOTOF LABEL_2 DEFAULT GOTOF LABEL_3",
            "N40 LABEL_1: X1",
            "N45 GOTOF ENDE",
            "N50 LABEL_2: X2",
            "N55 GOTOF ENDE",
            "N60 LABEL_3: X3",
            "N70 ENDE: M30",
        ]
    )
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [1.0, 1.0, 1.0]


def test_case_falls_back_to_default():
    program = "CASE(5) OF 7 GOTOF SEVEN DEFAULT GOTOF OTHER\nSEVEN: X7\nGOTOF ENDE\nOTHER: X0\nENDE: M30"
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [0.0, 0.0]


def test_case_without_default_falls_through():
    df, _state = nc_to_dataframe("CASE(5) OF 7 GOTOF SEVEN\nX1\nSEVEN: X2")
    assert df["X"].drop_nulls().to_list() == [1.0, 2.0]


def test_case_can_use_backward_jumps():
    """Instead of GOTOF, all other GOTO commands can be programmed (4.1.5.3)."""
    program = "TARGET1: X1\nR1=1\nCASE(R1) OF 1 GOTO ENDE 2 GOTOB TARGET1\nENDE: M30"
    df, _state = nc_to_dataframe(program)
    assert df["X"].drop_nulls().to_list() == [1.0, 1.0]


def test_label_names_may_shadow_keyword_prefixes():
    """A label starting with a keyword (GOTO_END) must parse as a label."""
    df, _state = nc_to_dataframe("GOTOF GOTO_END\nX999\nGOTO_END: X1")
    assert df["X"].to_list() == [1.0]
