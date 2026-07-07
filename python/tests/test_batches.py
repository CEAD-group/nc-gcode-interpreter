"""nc_to_batches: columnar streaming twin of nc_to_dataframe. Concatenating
the batches must reconstruct the batch DataFrame exactly (schema + values),
with memory bounded by the batch size and forward-fill carried across batch
boundaries."""

import pathlib

import polars as pl
import pytest
from nc_gcode_interpreter import nc_to_batches, nc_to_dataframe, nc_to_rows
from polars.testing import assert_frame_equal


def _concat(frames, columns):
    # New columns may first appear in a later batch, so the batch schemas grow;
    # a diagonal concat unions them (missing -> null) and the final select
    # restores the canonical column order of nc_to_dataframe.
    if not frames:
        return None
    return pl.concat(frames, how="diagonal").select(columns)


def assert_batches_match_dataframe(program, batch_size, **kwargs):
    df, state = nc_to_dataframe(program, **kwargs)
    it = nc_to_batches(program, batch_size=batch_size, **kwargs)
    frames = list(it)

    # Every batch is bounded by batch_size rows.
    assert all(f.height <= batch_size for f in frames)
    assert sum(f.height for f in frames) == df.height

    concatenated = _concat(frames, df.columns)
    if concatenated is None:
        assert df.height == 0
    else:
        assert_frame_equal(concatenated, df)

    # State is available after exhaustion, like nc_to_rows.
    assert it.state == state
    return frames


ARC = pathlib.Path(__file__).parent.parent.parent / "examples" / "arc.mpf"


@pytest.mark.parametrize("batch_size", [1, 2, 3, 5, 500_000])
def test_arc_batches_reconstruct_dataframe(batch_size):
    program = ARC.read_text()
    frames = assert_batches_match_dataframe(program, batch_size)
    if batch_size == 1:
        assert len(frames) > 1  # actually exercises multi-batch


def test_forward_fill_carries_across_batch_boundaries():
    # Y is set in batch 1 and must forward-fill into batch 2's rows even though
    # those rows carry no Y; Z first appears in a later batch (null before).
    program = "G1 X10 F100\nY1\nY2\nY3\nZ5\nY4\n"
    for batch_size in (1, 2, 3):
        assert_batches_match_dataframe(program, batch_size)


def test_block_addresses_appearing_in_a_later_batch():
    # I/J (never forward-filled) first appear on the arc block in a later batch.
    program = "G1 X0 Y0 F1000\nX1 Y1\nG2 X2 Y2 I1 J1\nX3 Y3\n"
    for batch_size in (1, 2):
        assert_batches_match_dataframe(program, batch_size)


def test_extra_axes_and_ic_match_dataframe():
    program = "G1 X0 Y0 A0 B0 C0 ELX=100 F1000\nX1 ELX=101\nX2 E=IC(1)\nX3\n"
    kwargs = dict(
        extra_axes=["ELX"],
        axis_index_map={"E": 4, "ELX": 5},
        allow_undefined_variables=True,
    )
    for batch_size in (1, 2, 3):
        assert_batches_match_dataframe(program, batch_size, **kwargs)


def test_single_batch_equals_dataframe_without_reordering():
    # When every column is present in the (only) batch, the batch schema already
    # matches nc_to_dataframe: a plain vertical concat is exact.
    program = "G1 X10 Y20 Z30 F1000\nX11 Y21 Z31\nX12 Y22 Z32\n"
    df, _ = nc_to_dataframe(program)
    frames = list(nc_to_batches(program, batch_size=500_000))
    assert len(frames) == 1
    assert_frame_equal(pl.concat(frames), df)


@pytest.mark.parametrize(
    "n_rows, batch_size, expected_batches, expected_last",
    [
        (10, 3, 4, 1),   # ceil(10/3)=4, last batch 10-9=1 row
        (10, 5, 2, 5),   # exact multiple: 2 full batches, no trailing empty one
        (10, 4, 3, 2),   # ceil(10/4)=3, last batch 2 rows
        (10, 100, 1, 10),  # batch_size > n_rows: one batch of all rows
        (1, 1, 1, 1),
    ],
)
def test_batch_count_and_boundaries(n_rows, batch_size, expected_batches, expected_last):
    # A program of exactly n_rows output rows (each a move) must split into
    # ceil(n_rows / batch_size) batches, all but the last of size batch_size and
    # the last carrying the remainder - never a trailing empty batch.
    program = "".join(f"X{i}\n" for i in range(n_rows))
    frames = list(nc_to_batches(program, batch_size=batch_size))
    assert len(frames) == expected_batches
    assert [f.height for f in frames] == (
        [batch_size] * (expected_batches - 1) + [expected_last]
    )
    assert sum(f.height for f in frames) == n_rows


def test_variable_only_rows_do_not_count_toward_batches():
    # Interleaved variable-only blocks (R1=...) carry no output cells and are
    # dropped, so they neither produce rows nor consume batch capacity: two
    # output rows with batch_size=1 yield exactly two single-row batches.
    program = "R1=1\nX1\nR2=2\nX2\nR3=3\n"
    frames = list(nc_to_batches(program, batch_size=1))
    assert [f.height for f in frames] == [1, 1]


def test_disable_forward_fill_matches_dataframe():
    program = "G1 X10 F100\nY1\nY2\nZ5\nY4\n"
    df, _ = nc_to_dataframe(program, disable_forward_fill=True)
    frames = list(
        nc_to_batches(program, batch_size=2, disable_forward_fill=True)
    )
    assert_frame_equal(pl.concat(frames, how="diagonal").select(df.columns), df)


def test_state_available_after_exhaustion():
    it = nc_to_batches("R1=5\nX=R1\nX=R1+1", batch_size=1)
    assert it.state is None  # not yet exhausted
    list(it)  # exhaust the iterator
    assert it.state is not None
    assert it.state["symbol_table"]["R1"] == 5.0


def test_interpreter_error_propagates_from_batches():
    # An undefined variable (without allow_undefined_variables) must raise a
    # ValueError while iterating, like nc_to_dataframe / nc_to_rows.
    with pytest.raises(ValueError):
        list(nc_to_batches("X10\nY=UNDEFINED_VAR\nX20", batch_size=1))


def test_invalid_batch_size_raises():
    with pytest.raises(ValueError):
        list(nc_to_batches("X10", batch_size=0))


def test_line_no_is_the_leading_column():
    df, _ = nc_to_dataframe("G1 X0 Y0 F100\nX10\nX20\n")
    assert df.columns[0] == "line_no"
    assert df["line_no"].dtype == pl.Int64
    # 1-based, one value per output row, never null.
    assert df["line_no"].to_list() == [1, 2, 3]
    assert df["line_no"].null_count() == 0


@pytest.mark.parametrize(
    "program, expected",
    [
        # WHILE loop: the body line repeats; the plain-variable line emits no
        # row; the line after the loop follows.
        ("R1=0\nWHILE R1<2\nX=R1\nR1=R1+1\nENDWHILE\nX9\n", [3, 3, 6]),
        # GOTO reorders execution: source lines appear in execution order,
        # non-monotonic across the jump.
        ("N10 X1\nGOTOF 40\nN30 X999\nN40 X4\n", [1, 4]),
    ],
)
def test_line_no_matches_streaming_under_loops_and_jumps(program, expected):
    # The batch/dataframe line_no must equal the streaming nc_to_rows line_no
    # row-for-row (repeated under loops, non-monotonic across jumps).
    df, _ = nc_to_dataframe(program)
    stream_lines = [line for line, *_ in nc_to_rows(program)]
    assert df["line_no"].to_list() == expected
    assert df["line_no"].to_list() == stream_lines


def test_line_no_survives_flattening():
    # Flatten-generated samples keep the originating block's source line, so a
    # flattened arc/spline block yields many rows all carrying its line_no.
    program = "G1 X0 Y0 F1000\nG2 X100 Y0 I50 J0\nG1 X100 Y50\n"
    df, _ = nc_to_dataframe(program, flatten_tolerance=0.1)
    assert df.columns[0] == "line_no"
    assert df["line_no"].null_count() == 0
    # The arc on line 2 expands to many G1 samples, every one tagged line 2.
    per_line = df["line_no"].to_list()
    assert per_line.count(2) > 5
    # And it still matches the streaming path row-for-row.
    stream_lines = [line for line, *_ in nc_to_rows(program, flatten_tolerance=0.1)]
    assert per_line == stream_lines


def test_line_no_reconstructs_across_batches():
    # Concatenating batches restores the same per-row line_no as the whole-file
    # dataframe, so batch boundaries never disturb the column.
    program = "R1=0\nWHILE R1<5\nX=R1 Y=R1\nR1=R1+1\nENDWHILE\nX9\n"
    df, _ = nc_to_dataframe(program)
    for batch_size in (1, 2, 3):
        frames = list(nc_to_batches(program, batch_size=batch_size))
        concatenated = pl.concat(frames, how="diagonal").select(df.columns)
        assert concatenated["line_no"].to_list() == df["line_no"].to_list()


def test_path_input_batches(tmp_path):
    program = "G1 X10 Y20\nX20 Y5\nX30 Y6\n"
    mpf = tmp_path / "prog.mpf"
    mpf.write_text(program)
    df, _ = nc_to_dataframe(program)
    frames = list(nc_to_batches(mpf, batch_size=2))
    assert_frame_equal(pl.concat(frames, how="diagonal").select(df.columns), df)
