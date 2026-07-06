"""Optional toolpath visualization via `threejs-viewer`.

Bridges an interpreted (ideally flattened) toolpath DataFrame to the
`threejs-viewer <https://pypi.org/project/threejs-viewer/>`_ package: the
path renders as an extruded bead tube with a draw-range animation that
replays the program at feed-rate-proportional speed, and — when the
``flattened`` marker column is present — programmed points and
flattener-generated samples get distinct colors.

Requires the ``viz`` extra::

    pip install 'nc-gcode-interpreter[viz]'

Typical use::

    from nc_gcode_interpreter import nc_to_dataframe
    from nc_gcode_interpreter.viz import view_toolpath

    df, _ = nc_to_dataframe(program, flatten_tolerance=0.1)
    view_toolpath(df)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import polars as pl

if TYPE_CHECKING:
    import numpy as np

#: Colors for the ``flattened`` marker: programmed positions vs generated
#: samples (packed 0x00RRGGBB).
PROGRAMMED_COLOR = 0xFF9500  # orange
FLATTENED_COLOR = 0x2E86DE  # blue

_INSTALL_HINT = (
    "threejs-viewer is not installed; visualization needs the viz extra: "
    "pip install 'nc-gcode-interpreter[viz]'"
)


def detect_bead_size(df: pl.DataFrame) -> tuple[float | None, float | None]:
    """Best-effort bead cross-section from header comments.

    CAM output often states the bead in comments (``;Deposition width: 6.00``,
    ``;Layer height: 1.50``), but the format is not standardized — this scans
    the first comments for ``...width: <n>`` and ``layer height: <n>``
    patterns and returns ``(width, height)`` with ``None`` for whatever was
    not found. Explicit arguments always win over this heuristic.
    """
    import re

    if "comment" not in df.columns:
        return None, None
    width = height = None
    # Header metadata sits in the first blocks; 200 comments is plenty.
    for comment in df["comment"].drop_nulls().head(200):
        if width is None:
            match = re.search(
                r"(?:deposition|bead|extrusion|line)[ _]?width\s*[:=]\s*([0-9]*\.?[0-9]+)",
                comment,
                re.IGNORECASE,
            )
            if match:
                width = float(match.group(1))
        if height is None:
            match = re.search(
                r"layer[ _]?height\s*[:=]\s*([0-9]*\.?[0-9]+)", comment, re.IGNORECASE
            )
            if match:
                height = float(match.group(1))
        if width is not None and height is not None:
            break
    return width, height


def toolpath_arrays(
    df: pl.DataFrame,
    *,
    bead_width: float | None = None,
    bead_height: float | None = None,
    default_feed: float = 1000.0,
) -> "tuple[np.ndarray, np.ndarray | None]":
    """Convert an interpreter DataFrame to threejs-viewer toolpath arrays.

    Returns ``(data, colors)``: ``data`` is the ``(N, 6)`` float32
    ``[t_s, x, y, z, width, height]`` array a
    :class:`threejs_viewer.Toolpath` is built from — timestamps integrate
    segment length over the ``F`` feed rate (mm/min, ``default_feed`` when
    absent), so the animation replays at programmed speed.

    Travel moves render as travel, not bead: when an ``E`` column is present,
    points whose arriving segment deposits no material (``dE <= 0``) get zero
    width/height — ``add_toolpath`` then splits the path into separate
    extrusion tubes instead of drawing rapids as solid streaks. Without an
    ``E`` column the whole path is treated as extrusion.

    The bead cross-section defaults to :func:`detect_bead_size` (header
    comments), falling back to 4.0 x 2.0; explicit arguments win.

    ``colors`` is a ``(N,)`` packed ``0x00RRGGBB`` array distinguishing
    programmed points (:data:`PROGRAMMED_COLOR`) from flattener-generated
    samples (:data:`FLATTENED_COLOR`) when the ``flattened`` column is
    present, else ``None``.

    Pure array work (no viewer, no websocket) — split out so it is testable
    and reusable without ``threejs-viewer`` installed.
    """
    import numpy as np

    n = df.height
    if n < 2:
        raise ValueError(f"toolpath needs at least 2 rows, got {n}")

    # Detect the bead from comments BEFORE dropping zero-length rows: the
    # header comments live on exactly the rows the dedupe removes.
    detected_width, detected_height = detect_bead_size(df)

    def column(name: str, default: float) -> np.ndarray:
        if name in df.columns:
            return df[name].fill_null(default).to_numpy().astype(np.float64)
        return np.full(n, default, dtype=np.float64)

    x = column("X", 0.0)
    y = column("Y", 0.0)
    z = column("Z", 0.0)
    feed_all = np.maximum(column("F", default_feed), 1e-6) / 60.0  # mm/min -> mm/s

    # Drop zero-length runs (comment / M-code rows carry forward-filled
    # coordinates): they add no geometry but produce degenerate tube frames
    # in the client-side mesh. Keep the last row of each run (its E value is
    # the total extruded there), but take the arriving feed from the run's
    # FIRST row - the one that actually performed the move; later rows of the
    # run may already program the feed of the next move.
    moved = (np.diff(x) != 0) | (np.diff(y) != 0) | (np.diff(z) != 0)
    kept = np.concatenate((moved, [True]))
    kept_indices = np.flatnonzero(kept)
    run_starts = np.concatenate(([0], kept_indices[:-1] + 1))
    feed = feed_all[run_starts]
    if not kept.all():
        df = df.filter(pl.Series(kept))
        n = df.height
        if n < 2:
            raise ValueError("toolpath collapses to fewer than 2 distinct points")
        x, y, z = x[kept], y[kept], z[kept]

    # Per-point arrival times from segment length over feed.
    seg = np.sqrt(np.diff(x) ** 2 + np.diff(y) ** 2 + np.diff(z) ** 2)
    t = np.zeros(n, dtype=np.float64)
    t[1:] = np.cumsum(seg / feed[1:])

    width = bead_width if bead_width is not None else detected_width
    if width is None:
        width = 4.0
    height = bead_height if bead_height is not None else detected_height
    if height is None:
        height = width / 2.0

    # Extrusion mask, mirroring threejs_viewer.Toolpath.from_gcode: a point
    # extrudes when its arriving segment increases E; zero-length connector
    # points inherit the departing segment so continuous extrusion is not
    # broken at joins. Point 0 stays a start cap.
    if "E" in df.columns:
        e = column("E", 0.0)
        arriving = np.diff(e, prepend=e[0]) > 1e-10
        zero_len = np.concatenate(([False], seg < 1e-10))
        departing = np.diff(e, append=e[-1]) > 1e-10
        extruding = arriving | (zero_len & departing)
    else:
        extruding = np.ones(n, dtype=bool)

    data = np.column_stack(
        [
            t,
            x,
            y,
            z,
            np.where(extruding, width, 0.0),
            np.where(extruding, height, 0.0),
        ]
    ).astype(np.float32)

    colors: np.ndarray | None = None
    if "flattened" in df.columns:
        generated = df["flattened"].fill_null(0.0).to_numpy().astype(bool)
        colors = np.where(generated, FLATTENED_COLOR, PROGRAMMED_COLOR).astype(np.uint32)
    return data, colors


def view_toolpath(
    df: pl.DataFrame,
    *,
    bead_width: float | None = None,
    bead_height: float | None = None,
    default_feed: float = 1000.0,
    speed: float = 1.0,
    scale: float = 0.001,
    id: str = "toolpath",
    viewer: Any = None,
    wait: bool = True,
    **tube_kwargs: Any,
) -> Any:
    """Show an interpreted toolpath in threejs-viewer, animated at feed rate.

    Renders ``df`` (as produced by :func:`~nc_gcode_interpreter.nc_to_dataframe`,
    ideally with ``flatten_tolerance`` so arcs/splines are polylines) as an
    extruded bead tube and loads a draw-range animation replaying the program
    at feed-rate-proportional speed (``speed=60`` plays a minute per second).
    With the ``flattened`` marker column present, programmed points render
    orange and generated samples blue; otherwise the bead gets a viridis
    arc-length gradient.

    Args:
        df: interpreted toolpath DataFrame (needs at least an ``X``/``Y``/``Z``
            subset; ``F`` drives the time base when present).
        bead_width / bead_height: tube cross-section in path units; when
            omitted they are detected from header comments
            (:func:`detect_bead_size`), falling back to 4.0 x 2.0.
        default_feed: feed (mm/min) assumed when ``F`` is absent.
        speed: animation time-lapse factor.
        scale: scene scale applied to all lengths (coordinates and bead).
            Defaults to 0.001 (mm -> m): threejs-viewer scenes are
            meter-scale, and a millimetre-sized scene wrecks the adaptive
            depth-buffer precision when zoomed in (surfaces z-fight).
        id: viewer object id (reuse to replace a previous path).
        viewer: an existing ``threejs_viewer`` client; a new one is started
            (opening the browser) when None.
        wait: block until the browser has loaded the scene.
        **tube_kwargs: forwarded to ``add_toolpath`` (``roughness``, ...).

    Returns:
        The viewer client, so callers can keep adding objects.
    """
    try:
        import numpy as np
        from threejs_viewer import Animation, Toolpath
        from threejs_viewer import viewer as start_viewer
    except ImportError as error:  # pragma: no cover - exercised without extra
        raise ImportError(_INSTALL_HINT) from error

    data, colors = toolpath_arrays(
        df, bead_width=bead_width, bead_height=bead_height, default_feed=default_feed
    )
    if speed != 1.0:
        data[:, 0] /= float(speed)
    if scale != 1.0:
        data[:, 1:6] *= float(scale)  # xyz + bead cross-section; time untouched
    toolpath = Toolpath(data)

    v = viewer if viewer is not None else start_viewer()
    if colors is not None:
        tube_kwargs.setdefault("colors", colors)
    else:
        toolpath.colorize("viridis")
    v.add_toolpath(id, toolpath, **tube_kwargs)

    # One keyframe per point: the drawn fraction grows linearly in point
    # index while frame times follow the feed-rate time base, so dense
    # flattened regions draw at the same physical speed as long G1 moves.
    animation = Animation(loop=True)
    animation.set_frame_times(toolpath.times)
    fractions = np.linspace(0.0, 1.0, len(toolpath), dtype=np.float32)
    animation.set_draw_range_data([id], fractions[:, None])
    v.load_animation(animation)

    if wait:
        v.wait_for_assets()
    return v
