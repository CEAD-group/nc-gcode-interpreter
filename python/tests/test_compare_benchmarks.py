"""Exercise scripts/compare_benchmarks.py on synthetic reports.

The script is stdlib-only and runs as a subprocess, so this test needs neither
the compiled extension nor a benchmark binary.
"""

import json
import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "compare_benchmarks.py"
INPUT = {"sha256": "a" * 64, "bytes": 1000, "lines": 50}
CASE = {
    "mode": "fresh",
    "shape": "flat",
    "lines": 10000,
    "edit": "none",
    "threads": 1,
    "block_size": 1024,
    "stage1": 1,
    "vm": 0,
}


def sample(rows=10000.0, output_digest="1" * 16, state_digest="2" * 16):
    return {
        "rows": rows,
        "output_digest": output_digest,
        "state_digest": state_digest,
    }


def report(*, verify_digest, input_identity=INPUT, samples=None, warm_ms=100.0):
    if samples is None:
        samples = [sample(), sample()]
    return {
        "input_identity": input_identity,
        "platform": "test",
        "cpu": "test",
        "arguments": {
            "batch_size": 4096,
            "cancel": False,
            "allow_undefined": False,
            "axis_index_e": None,
            "verify_digest": verify_digest,
        },
        "results": [
            dict(
                CASE,
                exit_code=0,
                peak_rss_bytes=1_000_000,
                samples=samples,
                warm_median_ms=warm_ms,
            )
        ],
    }


def compare(tmp_path, baseline, candidate):
    paths = []
    for name, content in (("baseline", baseline), ("candidate", candidate)):
        path = tmp_path / f"{name}.json"
        path.write_text(json.dumps(content))
        paths.append(str(path))
    return subprocess.run(
        [sys.executable, "-I", str(SCRIPT), *paths],
        capture_output=True,
        text=True,
    )


def test_identical_inputs_and_digests_compare(tmp_path):
    run = compare(
        tmp_path,
        report(verify_digest=True),
        report(verify_digest=True, warm_ms=50.0),
    )
    assert run.returncode == 0, run.stderr
    header, row = run.stdout.splitlines()
    assert header.endswith("baseline_ms,candidate_ms,time_delta_pct,rss_delta_pct")
    assert row.endswith("100.0,50.0,-50.0,0.0")
    assert "Compared 1 cases" in run.stderr
    assert "not cross-checked" not in run.stderr


def test_refuses_different_input_sha256(tmp_path):
    run = compare(
        tmp_path,
        report(verify_digest=True),
        report(verify_digest=True, input_identity=dict(INPUT, sha256="b" * 64)),
    )
    assert run.returncode != 0
    assert "inputs differ" in run.stderr
    assert "sha256" in run.stderr
    assert "baseline_ms" not in run.stdout


def test_refuses_synthetic_against_real_file(tmp_path):
    run = compare(
        tmp_path,
        report(verify_digest=False, input_identity=None),
        report(verify_digest=False),
    )
    assert run.returncode != 0
    assert "inputs differ" in run.stderr
    assert "synthetic" in run.stderr


def test_refuses_different_output_digest_with_digests_verified(tmp_path):
    run = compare(
        tmp_path,
        report(verify_digest=True),
        report(
            verify_digest=True,
            samples=[sample(), sample(output_digest="f" * 16)],
            warm_ms=50.0,
        ),
    )
    assert run.returncode != 0
    assert "correctness digests changed" in run.stderr
    assert "sample 1 output_digest" in run.stderr
    assert "baseline_ms" not in run.stdout


def test_refuses_different_state_digest_with_digests_verified(tmp_path):
    run = compare(
        tmp_path,
        report(verify_digest=True),
        report(verify_digest=True, samples=[sample(state_digest="f" * 16), sample()]),
    )
    assert run.returncode != 0
    assert "sample 0 state_digest" in run.stderr


def test_refuses_different_rows_without_digests(tmp_path):
    zero = "0" * 16
    run = compare(
        tmp_path,
        report(verify_digest=False, samples=[sample(10000.0, zero, zero)]),
        report(verify_digest=False, samples=[sample(9999.0, zero, zero)]),
    )
    assert run.returncode != 0
    assert "row counts changed" in run.stderr


def test_warns_when_digests_were_not_recorded(tmp_path):
    zero = "0" * 16
    run = compare(
        tmp_path,
        report(verify_digest=False, samples=[sample(10000.0, zero, zero)]),
        report(verify_digest=False, samples=[sample(10000.0, zero, zero)]),
    )
    assert run.returncode == 0, run.stderr
    assert "not cross-checked" in run.stderr
    assert len(run.stdout.splitlines()) == 2
