"""BSPLINE flattening honours the knot distances PL (#121).

The expected curves come from an independent de Boor evaluation on an
explicit clamped knot vector, not from the flattener.
"""

import itertools
import math

import polars as pl
import pytest
from nc_gcode_interpreter import nc_to_batches, nc_to_dataframe, nc_to_rows
from polars.testing import assert_frame_equal

START = (1733.115, 529.179)
# Control points and PL of the block measured on a Sinumerik ONE.
BLOCKS = [
    (1772.507, 536.125, 40),
    (1829.865, 618.040, 100),
    (1839.827, 617.169, 10),
    (1856.732, 653.421, 40),
    (1774.816, 596.063, 100),
    (1764.854, 596.935, 10),
    (1707.497, 515.020, 100),
    (1668.104, 508.074, 40),
    (1660.444, 501.646, 10),
]
TOLERANCE = 0.05


def program(blocks=BLOCKS, degree=3, pl_of=lambda i, value: value, head=""):
    """The measured block; `pl_of(i, value)` returns the PL of block i (from
    1) or None to leave it out."""
    lines = [f"G1 X{START[0]} Y{START[1]} F3000", f"BSPLINE SD={degree}"]
    if head:
        lines.append(head)
    for i, (x, y, value) in enumerate(blocks, start=1):
        knot_distance = pl_of(i, value)
        suffix = "" if knot_distance is None else f" PL={knot_distance}"
        lines.append(f"X{x} Y{y}{suffix}")
    lines.append("G1 X1599.160 Y450.223")
    return "\n".join(lines) + "\n"


def flatten(text, tolerance=TOLERANCE):
    df, _ = nc_to_dataframe(text, flatten_tolerance=tolerance)
    return df


def curve_points(df):
    """The flattened spline: every row but the leading and trailing G1."""
    return list(zip(df["X"].to_list()[1:-1], df["Y"].to_list()[1:-1], strict=True))


def clamped_knots(spans, degree):
    """Clamped knot vector whose interior knots are the running sums of
    `spans`."""
    knots = [0.0] * (degree + 1)
    total = 0.0
    for span in spans[:-1]:
        total += span
        knots.append(total)
    return knots + [total + spans[-1]] * (degree + 1)


def de_boor(ctrl, knots, degree, t):
    """Point of the B-spline at parameter t; `ctrl` rows are (x, y, w)."""
    k = degree
    while k < len(ctrl) - 1 and knots[k + 1] <= t:
        k += 1
    d = [[x * w, y * w, w] for x, y, w in ctrl[k - degree : k + 1]]
    for r in range(1, degree + 1):
        for j in range(degree, r - 1, -1):
            i = j + k - degree
            alpha = (t - knots[i]) / (knots[i + degree + 1 - r] - knots[i])
            d[j] = [(1 - alpha) * a + alpha * b for a, b in zip(d[j - 1], d[j], strict=True)]
    x, y, w = d[degree]
    return x / w, y / w


class Reference:
    """The B-spline on `ctrl` with the given knot spans."""

    def __init__(self, ctrl, spans, degree, samples=2000):
        self.ctrl = [(*point, 1.0) if len(point) == 2 else point for point in ctrl]
        assert len(spans) == len(self.ctrl) - degree
        self.degree = degree
        self.knots = clamped_knots(spans, degree)
        self.end = self.knots[-1]
        self.step = self.end / samples
        self.coarse = [self.at(self.step * i) for i in range(samples + 1)]

    def at(self, t):
        return de_boor(self.ctrl, self.knots, self.degree, min(max(t, 0.0), self.end))

    def distance(self, point):
        """Distance from `point` to the curve: the nearest coarse sample,
        refined by a ternary search around its parameter."""

        def dist(t):
            x, y = self.at(t)
            return math.hypot(x - point[0], y - point[1])

        nearest = min(
            range(len(self.coarse)),
            key=lambda i: (self.coarse[i][0] - point[0]) ** 2 + (self.coarse[i][1] - point[1]) ** 2,
        )
        lo, hi = (nearest - 1) * self.step, (nearest + 1) * self.step
        for _ in range(40):
            a, b = lo + (hi - lo) / 3, hi - (hi - lo) / 3
            if dist(a) < dist(b):
                hi = b
            else:
                lo = a
        return dist((lo + hi) / 2)

    def max_distance(self, points):
        return max(self.distance(point) for point in points)


def distance_to_polyline(point, polyline):
    best = math.inf
    px, py = point
    for (ax, ay), (bx, by) in itertools.pairwise(polyline):
        dx, dy = bx - ax, by - ay
        length2 = dx * dx + dy * dy
        t = 0.0 if length2 == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / length2))
        best = min(best, math.hypot(px - ax - t * dx, py - ay - t * dy))
    return best


CONTROL = [START] + [(x, y) for x, y, _ in BLOCKS]
CUBIC_SPANS = [value for _, _, value in BLOCKS[1:8]]
QUADRATIC_SPANS = [value for _, _, value in BLOCKS[0:8]]


def test_cubic_knot_spans_take_pl_of_blocks_2_to_8():
    points = curve_points(flatten(program()))
    assert len(points) > 50
    assert CUBIC_SPANS == [100, 10, 40, 100, 10, 100, 40]
    # The samples are points of the curve on the PL knots.
    assert Reference(CONTROL, CUBIC_SPANS, 3).max_distance(points) < 1e-6
    # The uniform-knot curve is a different curve.
    assert Reference(CONTROL, [1] * 7, 3).max_distance(points) > 1.0
    # The chords stay near the PL curve. The subdivision probes each chord
    # at three parameters, so the bound has the margin the Rust tests use.
    polyline = [START, *points]
    dense = Reference(CONTROL, CUBIC_SPANS, 3, samples=1500).coarse
    assert max(distance_to_polyline(point, polyline) for point in dense) < 1.5 * TOLERANCE
    # The curve ends on the last control point.
    assert points[-1] == CONTROL[-1]


def test_quadratic_knot_spans_take_pl_of_blocks_1_to_8():
    points = curve_points(flatten(program(degree=2)))
    assert QUADRATIC_SPANS == [40, 100, 10, 40, 100, 10, 100, 40]
    assert Reference(CONTROL, QUADRATIC_SPANS, 2).max_distance(points) < 1e-6
    assert Reference(CONTROL, [1] * 8, 2).max_distance(points) > 1.0


def test_first_and_last_pl_of_a_cubic_are_unused():
    changed = program(pl_of=lambda i, value: 7 if i in (1, 9) else value)
    assert_frame_equal(flatten(changed), flatten(program()))


def test_constant_pl_equals_no_pl(capfd):
    without = flatten(program(pl_of=lambda i, value: None))
    for constant in (1, 100, 0.3):
        for degree in (2, 3):
            expected = without if degree == 3 else flatten(program(degree=2, pl_of=lambda i, v: None))
            constant_pl = flatten(program(degree=degree, pl_of=lambda i, value, c=constant: c))
            assert_frame_equal(constant_pl, expected)
    assert capfd.readouterr().err == ""
    # Without PL the knots are uniform.
    assert Reference(CONTROL, [1] * 7, 3).max_distance(curve_points(without)) < 1e-6


def test_only_pl_ratios_count():
    doubled = flatten(program(pl_of=lambda i, value: 2 * value))
    assert_frame_equal(doubled, flatten(program()), abs_tol=1e-9)


def test_partial_pl_warns_and_uses_uniform_knots(capfd):
    without = flatten(program(pl_of=lambda i, value: None))
    assert capfd.readouterr().err == ""
    partial = flatten(program(pl_of=lambda i, value: None if i == 5 else value))
    err = capfd.readouterr().err
    assert err.count("BSPLINE PL values are ignored") == 1, err
    assert "line 3" in err and "not every control-point block carries a PL" in err
    assert_frame_equal(partial, without)


def test_pl_is_not_modal(capfd):
    # One PL on the first block does not carry to the following blocks.
    single = flatten(program(pl_of=lambda i, value: value if i == 1 else None))
    assert "not every control-point block" in capfd.readouterr().err
    assert_frame_equal(single, flatten(program(pl_of=lambda i, value: None)))


@pytest.mark.parametrize("degree", [1, 4])
def test_pl_with_other_degree_warns_and_uses_uniform_knots(capfd, degree):
    without = flatten(program(degree=degree, pl_of=lambda i, value: None))
    assert capfd.readouterr().err == ""
    with_pl = flatten(program(degree=degree))
    assert "only known for SD=2 and SD=3" in capfd.readouterr().err
    assert_frame_equal(with_pl, without)


def test_non_positive_pl_warns_and_uses_uniform_knots(capfd):
    without = flatten(program(pl_of=lambda i, value: None))
    with_zero = flatten(program(pl_of=lambda i, value: 0 if i == 4 else value))
    assert "not a positive number" in capfd.readouterr().err
    assert_frame_equal(with_zero, without)


def test_repeated_control_points_are_dropped_with_their_pl(capfd):
    expected = moves(flatten(program()))
    # A first block on the start position, and block 4 programmed twice.
    first = (START[0], START[1], 999)
    repeated = [*BLOCKS[:4], (BLOCKS[3][0], BLOCKS[3][1], 999), *BLOCKS[4:]]
    df = flatten(program(blocks=[first, *repeated]))
    assert capfd.readouterr().err == ""
    assert moves(df) == pytest.approx(expected)
    # A repeat at the end leaves the curve end on the last control point.
    df = flatten(program(blocks=[*BLOCKS, (BLOCKS[-1][0], BLOCKS[-1][1], 999)]))
    assert moves(df) == pytest.approx(expected)
    assert moves(df)[-1] == CONTROL[-1]


def moves(df):
    """The spline rows that move: a dropped block keeps a row for its other
    cells, which repeats the previous position."""
    points = [START, *curve_points(df)]
    return [b for a, b in itertools.pairwise(points) if a != b]


def test_pw_weights_apply_on_pl_knots():
    weights = {3: 2.5, 6: 0.4}
    lines = program().splitlines()
    for block, weight in weights.items():
        lines[block + 1] += f" PW={weight}"
    points = curve_points(flatten("\n".join(lines) + "\n"))
    ctrl = [(x, y, weights.get(i, 1.0)) for i, (x, y) in enumerate(CONTROL)]
    assert Reference(ctrl, CUBIC_SPANS, 3).max_distance(points) < 1e-6
    assert Reference(CONTROL, CUBIC_SPANS, 3).max_distance(points) > 0.5


@pytest.mark.parametrize("stage1,min_bytes", [("0", "0"), ("1", "0"), ("1", str(2**63))])
def test_pl_reaches_the_flattener_on_every_parse_path(monkeypatch, stage1, min_bytes):
    monkeypatch.setenv("NC_STAGE1", stage1)
    monkeypatch.setenv("NC_STREAM_MIN_BYTES", min_bytes)
    text = program()
    # Unflattened, PL is a per-block column that is not forward-filled.
    raw, _ = nc_to_dataframe(text)
    assert raw["PL"].to_list() == [None, None, *[float(v) for _, _, v in BLOCKS], None]
    spline = Reference(CONTROL, CUBIC_SPANS, 3)
    df = flatten(text, tolerance=0.1)
    batches = pl.concat(list(nc_to_batches(text, batch_size=7, flatten_tolerance=0.1)), how="diagonal")
    rows = [row for _, row in nc_to_rows(text, flatten_tolerance=0.1)]
    for points in (
        curve_points(df),
        curve_points(batches),
        [(row["X"], row["Y"]) for row in rows[1:-1]],
    ):
        assert len(points) > 20
        assert spline.max_distance(points) < 1e-6
    assert "PL" not in df.columns and "PL" not in batches.columns
