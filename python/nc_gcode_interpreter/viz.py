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


def toolpath_arrays(
    df: pl.DataFrame,
    *,
    bead_width: float = 4.0,
    bead_height: float | None = None,
    default_feed: float = 1000.0,
) -> "tuple[np.ndarray, np.ndarray | None]":
    """Convert an interpreter DataFrame to threejs-viewer toolpath arrays.

    Returns ``(data, colors)``: ``data`` is the ``(N, 6)`` float32
    ``[t_s, x, y, z, width, height]`` array a
    :class:`threejs_viewer.Toolpath` is built from — timestamps integrate
    segment length over the ``F`` feed rate (mm/min, ``default_feed`` when
    absent), so the animation replays at programmed speed. ``colors`` is a
    ``(N,)`` packed ``0x00RRGGBB`` array distinguishing programmed points
    (:data:`PROGRAMMED_COLOR`) from flattener-generated samples
    (:data:`FLATTENED_COLOR`) when the ``flattened`` column is present,
    else ``None``.

    Pure array work (no viewer, no websocket) — split out so it is testable
    and reusable without ``threejs-viewer`` installed.
    """
    import numpy as np

    n = df.height
    if n < 2:
        raise ValueError(f"toolpath needs at least 2 rows, got {n}")

    def column(name: str, default: float) -> np.ndarray:
        if name in df.columns:
            return df[name].fill_null(default).to_numpy().astype(np.float64)
        return np.full(n, default, dtype=np.float64)

    x = column("X", 0.0)
    y = column("Y", 0.0)
    z = column("Z", 0.0)

    # Per-point arrival times from segment length over feed (mm/min -> mm/s).
    feed = np.maximum(column("F", default_feed), 1e-6) / 60.0
    seg = np.sqrt(np.diff(x) ** 2 + np.diff(y) ** 2 + np.diff(z) ** 2)
    t = np.zeros(n, dtype=np.float64)
    t[1:] = np.cumsum(seg / feed[1:])

    height = bead_height if bead_height is not None else bead_width / 2.0
    data = np.column_stack(
        [
            t,
            x,
            y,
            z,
            np.full(n, bead_width, dtype=np.float64),
            np.full(n, height, dtype=np.float64),
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
    bead_width: float = 4.0,
    bead_height: float | None = None,
    default_feed: float = 1000.0,
    speed: float = 1.0,
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
        bead_width / bead_height: tube cross-section in path units
            (height defaults to half the width).
        default_feed: feed (mm/min) assumed when ``F`` is absent.
        speed: animation time-lapse factor.
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
