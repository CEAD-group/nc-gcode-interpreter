"""Differential test of NcSession (#47): random structure-free programs and
random edits, each resumed run compared with nc_to_batches of the edited text.

The reference never touches the checkpoint code, so a carried state the
snapshot misses shows up as a difference in rows, final state or events."""

import contextlib
import random

import polars as pl
import pytest
from nc_gcode_interpreter import NcSession, NotResumable, nc_to_batches
from polars.testing import assert_frame_equal

HEADER = [
    "R1=1 R2=2 R3=3 R4=4 R5=5",
    "DEF REAL QA=1, QB=2, QC=3, LEN=4",
    "G1 X0 Y0 Z0 A0 E0 F100",
]


def num(rng):
    return rng.choice([str(rng.randint(-200, 200)), f"{rng.uniform(-200, 200):.3f}", f"{rng.randint(0, 9)}.5"])


def motion(rng):
    words = [rng.choice(["G0", "G1", "", ""])]
    for axis in rng.sample(["X", "Y", "Z", "A", "E"], rng.randint(1, 3)):
        form = rng.random()
        if form < 0.75:
            words.append(f"{axis}{num(rng)}")
        elif form < 0.9:
            words.append(f"{axis}=IC({num(rng)})")
        else:
            words.append(f"{axis}=R{rng.randint(1, 5)}")
    if rng.random() < 0.2:
        words.append(f"F{rng.randint(100, 5000)}")
    if rng.random() < 0.2:
        words.insert(0, f"N{rng.randint(1, 9999)}")
    return " ".join(word for word in words if word)


def line(rng, failing):
    """One program line. `failing` adds lines that end or break the program."""
    radius = rng.randint(5, 40)
    name = rng.choice(["QA", "QB", "QC", "LEN"])
    pools = [
        (0.40, lambda: [motion(rng)]),
        (0.08, lambda: [f"R{rng.randint(1, 5)} = {expr}" for expr in ("R1 + 1", "R2 * 2", num(rng), "SIN(R3) + 2")]),
        (0.06, lambda: ["G90", "G91", "G17", "G18", "G19", "G64", "G60", "G94", "G500", "G54"]),
        (
            0.06,
            lambda: [
                f"TRANS X{num(rng)} Y{num(rng)}",
                f"ATRANS Z{num(rng)}",
                f"ROT Z{rng.randint(-90, 90)}",
                f"AROT X{rng.randint(-90, 90)}",
                "TRANS",
                "ROT",
                f"ROT RPL={rng.randint(-90, 90)}",
            ],
        ),
        (0.06, lambda: [f"{g} X=IC({2 * radius}) Y=IC(0) I{radius} J0" for g in ("G2", "G3")]),
        (
            0.06,
            lambda: [
                f"BSPLINE X{num(rng)} Y{num(rng)}",
                f"BSPLINE X{num(rng)} Y{num(rng)} PW=2",
                f"CSPLINE X{num(rng)} Y{num(rng)}",
                f"ASPLINE X{num(rng)} Y{num(rng)}",
                f"X{num(rng)} Y{num(rng)}",
                f"X{num(rng)} Y{num(rng)} Z{num(rng)}",
            ],
        ),
        (0.06, lambda: [f"T{rng.randint(1, 9)} M6", f"S{rng.randint(100, 9000)} M3", "M5", "M8 M9", 'T="TOOL_A"']),
        (
            0.06,
            lambda: [
                f"DEF REAL N{rng.randint(0, 10**6)}{name} = {num(rng)}",
                f'DEF STRING[20] S{rng.randint(0, 10**6)}{name} = "abc"',
                f"{name} = {num(rng)}",
                f"X={name}",
            ],
        ),
        (0.06, lambda: ["", "   ", "; comment", "; commentaire é ü 中", 'MSG("hello")', "G4 F1", "G1 X1 ; é"]),
        (0.04, lambda: [f"G1 X{num(rng)}\r", "G91\r", "; c\r", "\r"]),
    ]
    if failing:
        pools.append((0.03, lambda: ["M30", "M2", "M17", "G1 X==", "X=UNDEFINED_THING", "Q1 W2 U3", "G1 XY"]))
    pick = rng.random()
    for weight, pool in pools:
        if pick < weight:
            return rng.choice(pool())
        pick -= weight
    return motion(rng)


def program(rng, failing):
    lines = HEADER + [line(rng, failing) for _ in range(rng.randint(1, 80))]
    if rng.random() < 0.3:
        lines.append("M30")
    return "\n".join(lines) + ("\n" if rng.random() < 0.5 else "")


def edited(rng, text, failing):
    lines = text.split("\n")
    at = rng.randrange(len(lines))
    if rng.random() < 0.5:  # near the end, where a checkpoint precedes the edit
        at = max(0, len(lines) - 1 - rng.randint(0, max(1, len(lines) // 5)))
    kind = rng.random()
    if kind < 0.35:
        lines[at] = line(rng, failing)
    elif kind < 0.55:
        lines.insert(at, line(rng, failing))
    elif kind < 0.70 and len(lines) > 1:
        del lines[at]
    elif kind < 0.80:
        lines.append(line(rng, failing))
    elif kind < 0.85:
        return text
    elif kind < 0.90:
        return text.encode()[: rng.randrange(len(text) + 1)].decode(errors="ignore")
    elif kind < 0.95:
        lines[at] += " ; x"
    elif rng.random() < 0.5:
        lines[at:at] = ["IF R1 > 0", "G1 X1", "ENDIF"]
    else:
        lines[at:at] = ["GOTOF L1", "L1:"]
    return "\n".join(lines)


def outcome(iterator):
    """Frames, final state and events of a run, or the error it raised."""
    try:
        frames = list(iterator)
    except ValueError as error:
        return ("error", str(error), getattr(error, "line", None))
    return ("ok", frames, iterator.state, iterator.variable_events, iterator.variable_names)


def reference(kwargs, text):
    plain = {key: value for key, value in kwargs.items() if not key.startswith("checkpoint")}
    try:
        return outcome(nc_to_batches(text, **plain))
    except ValueError as error:
        return ("error", str(error), getattr(error, "line", None))


def table(frames):
    if not frames:
        return pl.DataFrame()
    frame = pl.concat(frames, how="diagonal_relaxed")
    return frame.select(sorted(frame.columns))


def assert_matches(got, start_row, want):
    assert got[0] == want[0], (got[:3], want[:3])
    if want[0] == "error":
        assert got == want
        return
    expected = table(want[1]).slice(start_row)
    rows = table(got[1])
    for column in expected.columns:
        if column not in rows.columns:
            # A column that only has values before `start_row`.
            assert expected[column].null_count() == expected.height, column
            rows = rows.with_columns(pl.lit(None, dtype=expected[column].dtype).alias(column))
    assert sorted(rows.columns) == sorted(expected.columns)
    assert rows.height == expected.height
    if rows.height:
        assert_frame_equal(rows.select(expected.columns), expected, check_exact=True, check_dtypes=False)
    assert got[2] == want[2]
    assert got[4] == want[4]
    if want[3] is None:
        assert got[3] is None
    else:
        assert_frame_equal(got[3], want[3], check_exact=True)


def session_kwargs(rng):
    kwargs = {
        "batch_size": rng.choice([1, 2, 3, 7, 50, 1000]),
        "include_line_numbers": rng.random() < 0.5,
        "include_variables": rng.random() < 0.5,
        "disable_forward_fill": rng.random() < 0.2,
        "allow_undefined_variables": rng.random() < 0.3,
    }
    if rng.random() < 0.5:
        kwargs["flatten_tolerance"] = rng.choice([0.01, 0.1, 1.0])
    if rng.random() < 0.3:
        kwargs["initial_state"] = "R1 = 5\nG1 F300 X0 Y0 Z0\nG91"
    if rng.random() < 0.5:
        kwargs["checkpoint_every_lines"] = rng.choice([1, 2, 5, 17])
        kwargs["checkpoint_every_rows"] = None
    else:
        kwargs["checkpoint_every_rows"] = rng.choice([1, 2, 5, 17, 60])
    return kwargs


@pytest.mark.parametrize("seed", range(4))
def test_resumed_runs_equal_a_plain_run_of_the_edited_text(seed):
    rng = random.Random(seed)
    resumed = 0
    for _ in range(60):
        failing = rng.random() < 0.3
        kwargs = session_kwargs(rng)
        text = program(rng, failing)
        session = NcSession(**kwargs)
        iterator = session.run(text)
        if rng.random() < 0.15:
            # An abandoned run leaves the checkpoints it took so far.
            with contextlib.suppress(ValueError):
                next(iterator, None)
            del iterator
        else:
            assert_matches(outcome(iterator), 0, reference(kwargs, text))
        for _ in range(rng.randint(1, 5)):
            text = edited(rng, text, failing)
            checkpoints = session.checkpoints()
            iterator = None
            if checkpoints and rng.random() < 0.3:
                with contextlib.suppress(NotResumable):
                    iterator = session.resume(rng.choice(checkpoints), text)
            if iterator is None:
                iterator = session.edit(text)
            resumed += iterator.resumed_from is not None
            if rng.random() < 0.1:
                with contextlib.suppress(ValueError):
                    next(iterator, None)
                del iterator
                continue
            assert_matches(outcome(iterator), iterator.start_row, reference(kwargs, text))
    assert resumed > 50, "the generator no longer exercises resume"


def test_checkpoint_count_is_bounded():
    """Past the cap a run keeps every second checkpoint and doubles its
    interval; the kept ones still resume exactly."""
    kwargs = {"batch_size": 500, "checkpoint_every_lines": 1, "include_variables": True}
    text = "\n".join(HEADER + [f"X{i % 500}.5 R1={i}" for i in range(5000)])
    session = NcSession(**kwargs)
    full = outcome(session.run(text))
    checkpoints = session.checkpoints()
    assert 256 < len(checkpoints) <= 1024
    assert checkpoints[-1].line_no > 4000
    appended = text + "\nG1 X1 Y2"
    want = reference(kwargs, appended)
    assert_matches(full, 0, reference(kwargs, text))
    for checkpoint in checkpoints[:: len(checkpoints) // 7]:
        iterator = session.resume(checkpoint, appended)
        assert_matches(outcome(iterator), iterator.start_row, want)
