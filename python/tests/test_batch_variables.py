"""Batch-path variable-change events (#46) + `string_table` in the state dict.

The streaming `nc_to_rows(include_variables=True)` path yields a per-row
`variables` dict; the batch path (`nc_to_batches` / `nc_to_dataframe`) now
surfaces the same information as a sparse side-table (`row_idx`, `name_id`,
`value`) plus a `variable_names` list. These tests pin (a) parity of the
ordered assignment sequence between the two paths and (b) that `DEF STRING`
variables round-trip into the returned state dict.
"""

import polars as pl

from nc_gcode_interpreter import nc_to_batches, nc_to_dataframe, nc_to_rows


def _stream_sequence(program: str) -> list[tuple[str, float]]:
    """Flat, ordered `(name, value)` assignment sequence from the stream."""
    return [
        (name, value)
        for _line, _row, variables in nc_to_rows(program, include_variables=True)
        for name, value in variables.items()
    ]


def _batch_sequence(it) -> list[tuple[str, float]]:
    """Flat, ordered `(name, value)` sequence decoded from the batch events."""
    events = it.variable_events
    names = it.variable_names
    return [
        (names[name_id], value)
        for name_id, value in zip(events["name_id"], events["value"])
    ]


def test_batch_variable_events_match_stream_simple():
    program = "\n".join(
        [
            "DEF REAL Q=2.5",  # variable-only
            "R1=0",            # variable-only
            "X=R1",            # output row, no change
            "X=Q",             # output row, no change
        ]
    )
    it = nc_to_batches(program, include_variables=True)
    list(it)  # exhaust to populate variable_events / state
    assert _batch_sequence(it) == _stream_sequence(program)


def test_batch_variable_events_match_stream_while_loop():
    # A WHILE loop where R1 changes once per iteration: the per-iteration
    # counter events must appear on the batch path exactly as they do on the
    # stream, in the same order.
    program = "\n".join(
        [
            "DEF REAL Q=2.5",
            "R1=0",
            "WHILE R1<3",
            "X=R1 Q=Q*2",  # output row that ALSO assigns a variable
            "R1=R1+1",     # variable-only, per iteration
            "ENDWHILE",
            "X=Q",
        ]
    )
    it = nc_to_batches(program, include_variables=True)
    list(it)
    assert _batch_sequence(it) == _stream_sequence(program)


def test_batch_variable_events_row_idx_reconstructs_symbol_table():
    # Replaying every event reconstructs the final symbol table (minus the
    # built-in TRUE/FALSE), mirroring the streaming accumulation invariant.
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
    it = nc_to_batches(program, include_variables=True)
    list(it)
    accumulated: dict[str, float] = {}
    for name, value in _batch_sequence(it):
        accumulated[name] = value
    symbol_table = dict(it.state["symbol_table"])
    for builtin in ("TRUE", "FALSE"):
        symbol_table.pop(builtin)
    assert accumulated == symbol_table

    # row_idx is monotonic non-decreasing (events arrive in program order) and
    # bounded by the number of output rows.
    events = it.variable_events
    row_idx = events["row_idx"].to_list()
    assert row_idx == sorted(row_idx)


def test_batch_variable_events_row_idx_aligns_with_output_rows():
    # A change on a variable-only block is attributed to the NEXT output row;
    # a change on an output row gets that row's own index.
    program = "\n".join(
        [
            "R1=0",   # variable-only, before output row 0
            "X=R1",   # output row 0
            "R1=1",   # variable-only, before output row 1
            "X=R1",   # output row 1
        ]
    )
    it = nc_to_batches(program, include_variables=True)
    list(it)
    events = it.variable_events
    names = it.variable_names
    decoded = [
        (row_idx, names[name_id], value)
        for row_idx, name_id, value in zip(
            events["row_idx"], events["name_id"], events["value"]
        )
    ]
    assert decoded == [(0, "R1", 0.0), (1, "R1", 1.0)]


def test_batch_variable_events_absent_without_flag():
    it = nc_to_batches("R1=5\nX=R1")
    list(it)
    assert it.variable_events is None
    assert it.variable_names is None


def test_batch_variable_events_empty_when_no_assignments():
    it = nc_to_batches("G1 X10\nX20 Y5", include_variables=True)
    list(it)
    assert isinstance(it.variable_events, pl.DataFrame)
    assert it.variable_events.height == 0
    assert it.variable_names == []


def test_string_table_round_trips_through_state_dict():
    program = "\n".join(
        [
            "DEF STRING[16] MSG",
            'MSG="HELLO WORLD"',
            "DEF STRING[8] TAG",
            'TAG="ABC"',
            "G1 X1",
        ]
    )
    _df, state = nc_to_dataframe(program)
    assert state["string_table"] == {"MSG": "HELLO WORLD", "TAG": "ABC"}
    # The existing numeric sub-tables are untouched by the new key.
    assert set(state) == {"axes", "symbol_table", "translation", "string_table"}
    # The string variable never leaks into the numeric symbol table.
    assert "MSG" not in state["symbol_table"]


def test_string_table_on_streaming_and_batch_iterators():
    program = 'DEF STRING[8] MSG\nMSG="HI"\nG1 X1'
    rows = nc_to_rows(program)
    list(rows)
    assert rows.state["string_table"] == {"MSG": "HI"}

    it = nc_to_batches(program)
    list(it)
    assert it.state["string_table"] == {"MSG": "HI"}


def test_line_numbers_and_variables_compose_on_the_batch_path():
    # include_line_numbers (#45) and include_variables (#46) share the batch
    # path and were merged independently; requesting both at once must give the
    # leading `line_no` column AND the sparse variable-event side-table, with
    # `row_idx` still aligned to the emitted (line_no-carrying) output rows.
    program = "\n".join(
        [
            "R1=0",          # variable-only, before output row 0
            "WHILE R1<3",
            "X=R1",          # output row (loop body, source line 3)
            "R1=R1+1",       # variable-only
            "ENDWHILE",
            "X9",            # output row, source line 6
        ]
    )
    it = nc_to_batches(
        program,
        batch_size=1_000_000,
        include_line_numbers=True,
        include_variables=True,
    )
    df = pl.concat(list(it), how="diagonal")

    # line_no leads and tracks source lines (loop body repeats line 3).
    assert df.columns[0] == "line_no"
    assert df["line_no"].to_list() == [3, 3, 3, 6]
    assert df["X"].to_list() == [0.0, 1.0, 2.0, 9.0]

    # Variable events still decode independently of the line_no column.
    names = it.variable_names
    decoded = [
        (row_idx, names[name_id], value)
        for row_idx, name_id, value in zip(
            it.variable_events["row_idx"],
            it.variable_events["name_id"],
            it.variable_events["value"],
        )
    ]
    assert decoded == [(0, "R1", 0.0), (1, "R1", 1.0), (2, "R1", 2.0), (3, "R1", 3.0)]
