"""NcSession checkpoints and resume (#47): resuming an edited structure-free
program from a checkpoint must yield exactly the batches, variable events and
final state a full run of the edited program yields from that row on."""

import pathlib
import time

import polars as pl
import pytest
from nc_gcode_interpreter import NcSession, NotResumable
from polars.testing import assert_frame_equal

EXAMPLES = pathlib.Path(__file__).parents[2] / "examples"


def drain(it):
    frames = list(it)
    events = it.variable_events
    return {
        "frames": frames,
        "state": it.state,
        "events": events,
        "names": it.variable_names,
    }


def frames_from(frames, start_row):
    """The frames of a full run from global row `start_row` on; a batch never
    straddles a checkpoint, so `start_row` is a batch boundary."""
    rows = 0
    for index, frame in enumerate(frames):
        if rows == start_row:
            return frames[index:]
        rows += frame.height
    assert rows == start_row, f"row {start_row} is not a batch boundary"
    return []


def assert_same_frames(got, want):
    assert len(got) == len(want)
    for a, b in zip(got, want, strict=True):
        assert a.schema == b.schema
        assert_frame_equal(a, b, check_exact=True)


def assert_resume_matches(kwargs, resumed_it, edited):
    """The resumed iterator equals a fresh full run of `edited` from its
    start row on: frames, final state and whole-program variable events."""
    start = resumed_it.start_row
    resumed = drain(resumed_it)
    full = drain(NcSession(**kwargs).run(edited))
    assert_same_frames(resumed["frames"], frames_from(full["frames"], start))
    assert resumed["state"] == full["state"]
    assert resumed["names"] == full["names"]
    if full["events"] is None:
        assert resumed["events"] is None
    else:
        assert_frame_equal(resumed["events"], full["events"], check_exact=True)
    return resumed, full


def generated(lines: int, edits: dict[int, str] | None = None) -> str:
    """Structure-free CAM-like program; `edits` maps 1-based line -> text."""
    edits = edits or {}
    out = []
    for i in range(1, lines + 1):
        if i in edits:
            out.append(edits[i])
        elif i == 1:
            out.append("G90 G1 F1000 X0 Y0 Z0 E0")
        elif i % 997 == 0:
            out.append(f"R{i % 7} = {i}")
        else:
            out.append(f"N{i} X{i % 500}.5 Y{(i * 7) % 300}.25 E=IC(0.0{i % 9 + 1})")
    return "\n".join(out)


MILLION = {
    "batch_size": 100_000,
    "include_line_numbers": True,
    "include_variables": True,
    "checkpoint_every_rows": 100_000,
}


@pytest.fixture(scope="module")
def million():
    text = generated(1_000_000)
    session = NcSession(**MILLION)
    return session, text, drain(session.run(text))


def test_million_line_resume_after_edit_matches_full_run(million):
    session, _, original = million
    checkpoints = session.checkpoints()
    assert len(checkpoints) >= 9
    assert all(c.row_idx % 100_000 == 0 for c in checkpoints)
    checkpoint = next(c for c in checkpoints if c.row_idx >= 500_000)
    edit_line = checkpoint.line_no + 10
    edited = generated(1_000_000, {edit_line: "N1 X-1 Y-1 R3=42"})
    it = session.resume(checkpoint, edited)
    assert (it.start_row, it.start_line) == (checkpoint.row_idx, checkpoint.line_no)
    resumed, full = assert_resume_matches(MILLION, it, edited)
    # The prefix is untouched: the original run's rows before the checkpoint
    # are the edited program's rows before it.
    prefix_rows = checkpoint.row_idx
    assert_frame_equal(
        pl.concat(original["frames"]).head(prefix_rows),
        pl.concat(full["frames"]).head(prefix_rows),
    )
    first = resumed["frames"][0]
    assert first["line_no"][0] == checkpoint.line_no
    assert first.filter(pl.col("line_no") == edit_line)["X"].to_list() == [-1.0]


def test_million_line_end_of_file_edit_is_fast(million):
    _, text, _ = million
    session = NcSession(**MILLION)
    start = time.perf_counter()
    drain(session.run(text))
    full_seconds = time.perf_counter() - start
    edited = text + "\nG1 X1 Y1"
    start = time.perf_counter()
    it = session.edit(edited)
    resumed = drain(it)
    resume_seconds = time.perf_counter() - start
    assert it.resumed_from is not None
    assert it.start_row >= 900_000
    print(
        f"\n1M lines: full run {full_seconds:.3f} s, end-of-file edit resume "
        f"{resume_seconds:.3f} s ({sum(f.height for f in resumed['frames'])} rows re-run)"
    )
    assert resume_seconds < full_seconds


@pytest.mark.parametrize(
    "edit, column, value",
    [("G1 F777", "F", 777.0), ("G91", "gg14_wp_measure_mode", "G91")],
)
def test_modal_change_after_checkpoint_propagates_to_every_suffix_row(edit, column, value):
    kwargs = {"batch_size": 50, "checkpoint_every_rows": 50, "include_line_numbers": True}
    text = generated(400)
    session = NcSession(**kwargs)
    drain(session.run(text))
    checkpoint = session.checkpoint_for(200)
    edit_line = checkpoint.line_no + 5
    edited = generated(400, {edit_line: edit})
    resumed, _ = assert_resume_matches(kwargs, session.resume(checkpoint, edited), edited)
    suffix = pl.concat(resumed["frames"]).filter(pl.col("line_no") >= edit_line)
    assert suffix.height > 100
    assert suffix[column].to_list() == [value] * suffix.height


def test_edit_before_checkpoint_is_refused_and_edit_falls_back():
    kwargs = {
        "batch_size": 20,
        "checkpoint_every_rows": 20,
        "include_variables": True,
        "initial_state": "R1 = 5\nG1 F300 X0 Y0 Z0 E0",
    }
    text = generated(300)
    session = NcSession(**kwargs)
    drain(session.run(text))
    late = session.checkpoint_for(250)
    edited = generated(300, {100: "G1 X7"})
    with pytest.raises(NotResumable) as error:
        session.resume(late, edited)
    assert error.value.reason == "prefix_changed"
    it = session.edit(edited)
    assert it.resumed_from is not None and it.resumed_from.line_no <= 100
    assert_resume_matches(kwargs, it, edited)
    # An edit on line 1 has no checkpoint before it: a full run.
    first = generated(300, {1: "G90 G1 F10 X1 Y0 Z0"})
    it = session.edit(first)
    assert it.resumed_from is None and it.start_row == 0
    assert_resume_matches(kwargs, it, first)


def test_session_keeps_checkpoints_across_resumes():
    kwargs = {"batch_size": 10, "checkpoint_every_rows": 10}
    session = NcSession(**kwargs)
    drain(session.run(generated(100)))
    before = [(c.line_no, c.row_idx) for c in session.checkpoints()]
    edited = generated(100, {95: "G1 X3"})
    drain(session.edit(edited))
    assert [(c.line_no, c.row_idx) for c in session.checkpoints()] == before
    again = generated(100, {50: "G1 X4", 95: "G1 X3"})
    it = session.edit(again)
    assert it.resumed_from.line_no <= 50
    assert_resume_matches(kwargs, it, again)


def test_loop_programs_have_no_checkpoints_and_refuse_resume():
    kwargs = {"batch_size": 5, "checkpoint_every_lines": 1}
    loop = (EXAMPLES / "loop.mpf").read_text()
    session = NcSession(**kwargs)
    drain(session.run(loop))
    assert session.checkpoints() == []
    it = session.edit(loop + "\nG1 X1")
    assert it.resumed_from is None
    drain(it)

    session = NcSession(**kwargs)
    text = generated(30)
    drain(session.run(text))
    checkpoint = session.checkpoint_for(10)
    for insert, reason in [
        ("WHILE R1 < 2\nR1 = R1 + 1\nENDWHILE", "structured"),
        ("LABEL1:\nGOTOB LABEL1", "jumps"),
    ]:
        with pytest.raises(NotResumable) as error:
            session.resume(checkpoint, generated(30, {20: insert}))
        assert error.value.reason == reason
    with pytest.raises(NotResumable) as error:
        NcSession(**kwargs).resume(checkpoint, text)
    assert error.value.reason == "foreign_checkpoint"


def test_parse_error_after_checkpoint_raises_during_iteration():
    session = NcSession(batch_size=10, checkpoint_every_rows=10)
    drain(session.run(generated(60)))
    checkpoint = session.checkpoint_for(40)
    it = session.resume(checkpoint, generated(60, {50: "G1 X=="}))
    with pytest.raises(ValueError) as error:
        list(it)
    assert error.value.line == 50


@pytest.mark.parametrize("path", sorted(EXAMPLES.rglob("*.mpf")), ids=lambda p: p.name)
def test_every_checkpoint_of_the_examples_resumes_exactly(path):
    """Real test programs, a checkpoint at every line: appending a block and
    resuming from each checkpoint equals the full run of the edited text."""
    kwargs = {
        "batch_size": 3,
        "checkpoint_every_lines": 1,
        "include_line_numbers": True,
        "include_variables": True,
        "allow_undefined_variables": True,
    }
    if path.parent.name == "flattening":
        kwargs["flatten_tolerance"] = 0.1
    text = path.read_text()
    session = NcSession(**kwargs)
    try:
        drain(session.run(text))
    except ValueError:
        pytest.skip("the example fails on purpose")
    checkpoints = session.checkpoints()
    if not checkpoints:
        pytest.skip("not structure-free")
    edited = text.replace("M30", "") + "\nG1 X12.5 R9=3\n"
    for checkpoint in checkpoints:
        assert_resume_matches(kwargs, session.resume(checkpoint, edited), edited)
