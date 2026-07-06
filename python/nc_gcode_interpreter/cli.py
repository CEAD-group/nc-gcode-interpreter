"""``nc-view``: interpret an NC program and show the animated toolpath.

Console-script entry point (``[project.scripts]``) for the ``viz`` extra:
interprets the program, flattens arcs/splines to a polyline, and opens
`threejs-viewer <https://pypi.org/project/threejs-viewer/>`_ with the bead
tube animated at feed-rate-proportional speed. Programmed points render
orange, flattener-generated samples blue.
"""

from __future__ import annotations

import argparse
from pathlib import Path


def _parse_axis_index_map(spec: str) -> dict[str, int]:
    try:
        pairs = [pair.split(":") for pair in spec.split(",") if pair.strip()]
        return {name.strip().upper(): int(index) for name, index in pairs}
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            f"expected comma-separated name:index pairs like 'E:4,X:0', got {spec!r}"
        ) from error


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="nc-view",
        description=(
            "Interpret a Sinumerik-flavored NC program and show the toolpath "
            "as an animated bead tube in threejs-viewer (opens a browser)."
        ),
    )
    parser.add_argument("input", type=Path, help="NC program file (.mpf)")
    parser.add_argument(
        "--flatten-tolerance",
        type=float,
        default=0.1,
        metavar="MM",
        help="max deviation when flattening arcs/splines to G1 polylines "
        "(default: %(default)s; use --no-flatten to view curves as raw endpoints)",
    )
    parser.add_argument(
        "--no-flatten",
        action="store_true",
        help="skip curve flattening (arcs/splines render as straight lines between endpoints)",
    )
    parser.add_argument(
        "--speed",
        type=float,
        default=60.0,
        metavar="FACTOR",
        help="animation time-lapse factor: 60 plays one minute of machine time per second (default: %(default)s)",
    )
    parser.add_argument(
        "--bead-width",
        type=float,
        default=None,
        metavar="MM",
        help="tube cross-section width (default: detected from header comments, else 4.0)",
    )
    parser.add_argument(
        "--bead-height",
        type=float,
        default=None,
        metavar="MM",
        help="tube cross-section height (default: detected from header comments, else half the width)",
    )
    parser.add_argument(
        "--default-feed",
        type=float,
        default=1000.0,
        metavar="MM_MIN",
        help="feed rate assumed when the program sets no F (default: %(default)s)",
    )
    parser.add_argument(
        "--follow",
        choices=["off", "follow", "lookat"],
        default="follow",
        help="camera tracking of the path tip during playback: 'follow' moves the "
        "camera along with the nozzle, 'lookat' turns it in place, 'off' leaves it "
        "free (the viewer's T button cycles modes at runtime) (default: %(default)s)",
    )
    parser.add_argument(
        "--no-travels",
        action="store_true",
        help="hide the thin 1px travel-move lines (drawn and animated with the bead by default)",
    )
    parser.add_argument(
        "--scale",
        type=float,
        default=0.001,
        metavar="FACTOR",
        help="scene scale applied to all lengths (default: %(default)s = mm to m; "
        "threejs-viewer is meter-scale and mm-sized scenes z-fight when zoomed in)",
    )
    parser.add_argument(
        "--extra-axes",
        type=lambda s: [a.strip() for a in s.split(",")],
        default=None,
        metavar="A,B",
        help="extra axis identifiers, comma-separated (as in the interpreter CLI)",
    )
    parser.add_argument(
        "--axis-index-map",
        type=_parse_axis_index_map,
        default=None,
        metavar="E:4,X:0",
        help="axis-to-index mapping for array assignments like FL[E]=10, "
        "comma-separated name:index pairs (as in the interpreter CLI)",
    )
    parser.add_argument(
        "--allow-undefined-variables",
        action="store_true",
        help="initialize undefined variables to 0.0 instead of erroring (as in the interpreter CLI)",
    )
    parser.add_argument(
        "--initial-state",
        type=Path,
        default=None,
        metavar="FILE",
        help="initial-state MPF file (variables / start positions)",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    try:
        from .viz import view_toolpath
        import threejs_viewer  # noqa: F401  - fail here, before interpreting
    except ImportError:
        print(
            "nc-view needs the viz extra: pip install 'nc-gcode-interpreter[viz]'",
        )
        return 1

    from . import nc_to_dataframe

    tolerance = None if args.no_flatten else args.flatten_tolerance
    initial_state = args.initial_state.read_text() if args.initial_state else None
    df, _state = nc_to_dataframe(
        args.input,
        initial_state=initial_state,
        extra_axes=args.extra_axes,
        axis_index_map=args.axis_index_map,
        allow_undefined_variables=args.allow_undefined_variables,
        flatten_tolerance=tolerance,
    )
    if df.height < 2:
        print(f"{args.input}: program produced {df.height} output row(s) - nothing to plot")
        return 1

    from .viz import detect_bead_size

    if args.bead_width is None or args.bead_height is None:
        detected_width, detected_height = detect_bead_size(df)
        found = [
            f"{label} {value}"
            for label, value in (("width", detected_width), ("height", detected_height))
            if value is not None
        ]
        if found:
            print(f"bead size from program comments: {', '.join(found)}")

    generated = int(df["flattened"].sum() or 0) if "flattened" in df.columns else 0
    print(
        f"{args.input}: {df.height} points"
        + (f" ({generated} from flattened curves, tolerance {tolerance})" if generated else "")
    )
    print("opening threejs-viewer (close the browser tab or Ctrl+C to stop) ...")
    view_toolpath(
        df,
        bead_width=args.bead_width,
        bead_height=args.bead_height,
        default_feed=args.default_feed,
        speed=args.speed,
        scale=args.scale,
        follow=None if args.follow == "off" else args.follow,
        travels=not args.no_travels,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
