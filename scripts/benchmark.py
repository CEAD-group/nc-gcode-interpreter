#!/usr/bin/env python3
"""Isolated release benchmark matrix. Standard library only; macOS/Linux.

Each process reports cold and warm samples; wait4 supplies per-process peak RSS.
No timing assertions: compare results on the same idle host/build configuration.
"""

import argparse
import csv
import hashlib
import io
import json
import os
import platform
import random
import statistics
import subprocess
import tempfile
import time
from datetime import UTC, datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def cpu_name():
    try:
        return (
            command("sysctl", "-n", "machdep.cpu.brand_string")
            if platform.system() == "Darwin"
            else platform.processor()
        )
    except subprocess.CalledProcessError:
        return platform.machine()


def input_identity(path):
    if path is None:
        return None
    digest = hashlib.sha256()
    line_count = 0
    last = b""
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
            line_count += chunk.count(b"\n")
            last = chunk[-1:]
    return {
        "sha256": digest.hexdigest(),
        "bytes": path.stat().st_size,
        "lines": line_count + int(bool(last) and last != b"\n"),
    }


def source_fingerprint():
    digest = hashlib.sha256()
    paths = [
        ROOT / "Cargo.toml",
        ROOT / "Cargo.lock",
        ROOT / "build.rs",
        Path(__file__).resolve(),
    ]
    paths += (
        list((ROOT / "src").rglob("*.rs"))
        + list((ROOT / "src").rglob("*.pest"))
        + list((ROOT / "benches").rglob("*.rs"))
    )
    for path in sorted(paths):
        digest.update(str(path.relative_to(ROOT)).encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def build():
    run = subprocess.run(
        [
            "cargo",
            "bench",
            "--features",
            "bench-experiments",
            "--bench",
            "workloads",
            "--no-run",
            "--message-format=json",
        ],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        check=True,
    )
    for line in run.stdout.splitlines():
        item = json.loads(line)
        if (
            item.get("reason") == "compiler-artifact"
            and item["target"]["name"] == "workloads"
            and item.get("executable")
        ):
            return item["executable"]
    raise RuntimeError("benchmark executable missing")


def measure(binary, case, args):
    env = dict(
        os.environ,
        NC_STAGE1=str(case["stage1"]),
        NC_VM=str(case["vm"]),
        RAYON_NUM_THREADS=str(case["threads"]),
    )
    env.pop("NC_STAGE1_STATS", None)
    env["NC_BENCH_VERIFY_DIGEST"] = str(int(args.verify_digest))
    env["NC_BENCH_ALLOW_UNDEFINED"] = str(int(args.allow_undefined))
    env.pop("NC_BENCH_AXIS_E_INDEX", None)
    if args.axis_index_e is not None:
        env["NC_BENCH_AXIS_E_INDEX"] = str(args.axis_index_e)
    cmd = [
        binary,
        case["mode"],
        case["shape"],
        str(case["lines"]),
        str(args.samples),
        str(case["block_size"]),
        case["edit"],
        str(args.batch_size),
        str(int(args.cancel)),
    ]
    # Files avoid pipe backpressure while wait4 obtains this child's own RSS.
    with (
        tempfile.TemporaryFile(mode="w+") as stdout,
        tempfile.TemporaryFile(mode="w+") as stderr,
    ):
        process = subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=stdout, stderr=stderr)
        deadline = time.monotonic() + args.timeout
        timed_out = False
        while True:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
            if pid:
                break
            if time.monotonic() > deadline:
                process.kill()
                _, status, usage = os.wait4(process.pid, 0)
                timed_out = True
                break
            time.sleep(0.05)
        process.returncode = os.waitstatus_to_exitcode(status)
        stdout.seek(0)
        stderr.seek(0)
        output, errors = stdout.read(), stderr.read()
    result = dict(
        case,
        exit_code=process.returncode,
        peak_rss_bytes=int(
            usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024)
        ),
    )
    if process.returncode:
        return dict(result, error=("timeout\n" if timed_out else "") + errors[-4000:])
    samples = [
        {k: v if k.endswith("_digest") else float(v) for k, v in row.items()}
        for row in csv.DictReader(io.StringIO(output))
    ]
    warm = [s["prepare_ms"] + s["run_ms"] for s in samples[1:]]
    result.update(
        stderr_tail=errors[-4000:],
        samples=samples,
        cold_ms=samples[0]["prepare_ms"] + samples[0]["run_ms"],
        warm_median_ms=statistics.median(warm),
        warm_min_ms=min(warm),
        warm_max_ms=max(warm),
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--verify-digest",
        action="store_true",
        help="hash output batches and final state; adds work to measured time",
    )
    parser.add_argument(
        "--input",
        type=Path,
        help="local real-world file; replaces synthetic shapes and sizes",
    )
    parser.add_argument("--allow-undefined", action="store_true")
    parser.add_argument("--axis-index-e", type=int)
    parser.add_argument("--lines", type=int, nargs="+", default=[10_000, 100_000])
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument(
        "--shapes",
        nargs="+",
        default=["flat", "calls", "grammar", "structured", "frames"],
        choices=["flat", "calls", "grammar", "structured", "frames"],
    )
    parser.add_argument(
        "--modes",
        nargs="+",
        default=[
            "fresh",
            "grammar-cache",
            "decoded-cache",
            "bounded",
            "fused",
        ],
        choices=["fresh", "grammar-cache", "decoded-cache", "bounded", "fused"],
    )
    parser.add_argument(
        "--edits",
        nargs="+",
        default=["none", "coordinate"],
        choices=["none", "coordinate", "parameter", "translation", "insert", "delete"],
    )
    parser.add_argument("--threads", nargs="+", type=int, default=[1, 4])
    parser.add_argument("--blocks", nargs="+", type=int, default=[64, 1024, 16384])
    parser.add_argument("--batch-size", type=int, default=4096)
    parser.add_argument(
        "--timeout", type=float, default=300, help="seconds per isolated case"
    )
    parser.add_argument(
        "--cancel",
        action="store_true",
        help="drop consumer after first batch; run_ms includes producer unwind",
    )
    parser.add_argument(
        "--reference",
        action="store_true",
        help="also run fresh whole-file grammar and VM",
    )
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.verify_digest and args.cancel:
        parser.error("digest validation requires complete output, not cancellation")
    if (
        args.timeout <= 0
        or min(args.lines) < 8
        or min([args.samples, args.batch_size, *args.threads, *args.blocks]) < 1
    ):
        parser.error("positive sizes required; at least eight source lines")
    if args.input and args.edits != ["none"]:
        parser.error(
            "real-file runs require --edits none; synthetic fixtures define the controlled edits"
        )
    if args.input:
        args.shapes = ["file:" + str(args.input.resolve())]
        args.lines = [0]  # actual bytes/row count come from executable
    binary = build()
    cases = []
    for lines in args.lines:
        for shape in args.shapes:
            for mode in args.modes:
                if mode in ("bounded", "fused", "decoded-cache") and shape != "flat":
                    continue
                for edit in args.edits:
                    if edit == "translation" and shape != "frames":
                        continue
                    for threads in args.threads if mode == "fresh" else [1]:
                        for block in args.blocks if mode == "decoded-cache" else [1024]:
                            variants = [(1, 0)]
                            if mode == "fresh" and args.reference and threads == 1:
                                variants += [(0, 0), (0, 1)]
                            for stage1, vm in variants:
                                cases.append(
                                    {
                                        "mode": mode,
                                        "shape": shape,
                                        "lines": lines,
                                        "edit": edit,
                                        "threads": threads,
                                        "block_size": block,
                                        "stage1": stage1,
                                        "vm": vm,
                                    }
                                )
    if not cases:
        parser.error("no supported cases for the selected modes and input shapes")
    random.Random(42).shuffle(cases)
    report = {
        "schema_version": 1,
        "input_identity": input_identity(args.input),
        "source_sha256": source_fingerprint(),
        "binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
        "timestamp": datetime.now(UTC).isoformat(),
        "commit": command("git", "rev-parse", "HEAD"),
        "dirty": command("git", "status", "--porcelain"),
        "platform": platform.platform(),
        "cpu": cpu_name(),
        "cpu_count": os.cpu_count(),
        "rustc": command("rustc", "-Vv"),
        "build_env": {
            key: os.environ.get(key)
            for key in [
                "RUSTFLAGS",
                "CARGO_PROFILE_RELEASE_LTO",
                "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                "CARGO_TARGET_DIR",
            ]
        },
        "arguments": vars(args)
        | {
            "output": str(args.output),
            "input": str(args.input) if args.input else None,
        },
        "results": [],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    observations = {}
    for i, case in enumerate(cases):
        result = measure(binary, case, args)
        if args.verify_digest and result["exit_code"] == 0:
            key = (case["shape"], case["lines"], case["edit"])
            observed = [
                (s["rows"], s["output_digest"], s["state_digest"])
                for s in result["samples"]
            ]
            if key in observations and observations[key] != observed:
                result.update(
                    exit_code=1, error="differential output/final-state digest mismatch"
                )
            observations.setdefault(key, observed)
        report["results"].append(result)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        print(
            f"{i + 1}/{len(cases)} {case}: {result.get('warm_median_ms', 'FAILED')} ms",
            flush=True,
        )
    if any(r["exit_code"] for r in report["results"]):
        raise SystemExit("Some cases failed; see report")


if __name__ == "__main__":
    main()
