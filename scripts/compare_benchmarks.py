#!/usr/bin/env python3
"""Compare matching benchmark cases; positive deltas mean slower/larger.

This reports evidence, not a statistical significance test or a CI gate.
"""

import argparse
import csv
import json
import sys
from pathlib import Path

CASE_FIELDS = (
    "mode",
    "shape",
    "lines",
    "edit",
    "threads",
    "block_size",
    "stage1",
    "vm",
)
IDENTITY_FIELDS = ("sha256", "bytes", "lines")
CORRECTNESS_FIELDS = ("rows", "output_digest", "state_digest")


def input_mismatch(old, new):
    """Describe how the benchmarked inputs differ, or None when they match."""
    if old is None and new is None:
        return None
    if old is None or new is None:
        synthetic = "baseline" if old is None else "candidate"
        return (
            f"the {synthetic} report ran the synthetic workloads, the other a real file"
        )
    if old["sha256"] == new["sha256"]:
        return None
    differing = [f for f in IDENTITY_FIELDS if old.get(f) != new.get(f)]
    return "; ".join(f"{f} {old.get(f)} vs {new.get(f)}" for f in differing)


def correctness_mismatch(old, new):
    """Describe the first sample whose rows or digests differ, or None."""
    for index, (a, b) in enumerate(zip(old["samples"], new["samples"], strict=False)):
        for field in CORRECTNESS_FIELDS:
            if a[field] != b[field]:
                return f"sample {index} {field}: {a[field]} vs {b[field]}"
    if len(old["samples"]) != len(new["samples"]):
        return f"sample counts: {len(old['samples'])} vs {len(new['samples'])}"
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    baseline = json.loads(args.baseline.read_text())
    candidate = json.loads(args.candidate.read_text())
    for field in ("platform", "cpu"):
        if baseline[field] != candidate[field]:
            print(
                f"Warning: different {field}; results may not be comparable",
                file=sys.stderr,
            )
    for field in (
        "batch_size",
        "cancel",
        "allow_undefined",
        "axis_index_e",
        "axis_index_elx",
        "verify_digest",
    ):
        if baseline["arguments"].get(field) != candidate["arguments"].get(field):
            parser.error(f"incompatible benchmark setting: {field}")
    mismatch = input_mismatch(
        baseline.get("input_identity"), candidate.get("input_identity")
    )
    if mismatch:
        parser.error(f"benchmark inputs differ: {mismatch}")
    verified = bool(baseline["arguments"].get("verify_digest"))
    if not verified:
        print(
            "Warning: correctness not cross-checked (row counts only); "
            "record both reports with --verify-digest to compare digests",
            file=sys.stderr,
        )
    previous = {
        tuple(row[key] for key in CASE_FIELDS): row
        for row in baseline["results"]
        if row["exit_code"] == 0
    }
    # Refuse before writing any CSV so a rejected comparison leaves no partial table.
    rows = []
    for row in candidate["results"]:
        key = tuple(row[key] for key in CASE_FIELDS)
        if key not in previous or row["exit_code"] != 0:
            continue
        old = previous[key]
        if verified:
            mismatch = correctness_mismatch(old, row)
            if mismatch:
                parser.error(
                    f"correctness digests changed for {key} ({mismatch}); "
                    "check correctness first"
                )
        elif {s["rows"] for s in old["samples"]} != {s["rows"] for s in row["samples"]}:
            parser.error(
                f"output row counts changed for {key}; check correctness first"
            )
        rows.append(
            (
                *key,
                round(old["warm_median_ms"], 3),
                round(row["warm_median_ms"], 3),
                round(100 * (row["warm_median_ms"] / old["warm_median_ms"] - 1), 2),
                round(100 * (row["peak_rss_bytes"] / old["peak_rss_bytes"] - 1), 2),
            )
        )
    if not rows:
        parser.error("no successful matching cases")
    writer = csv.writer(sys.stdout)
    writer.writerow(
        (*CASE_FIELDS, "baseline_ms", "candidate_ms", "time_delta_pct", "rss_delta_pct")
    )
    writer.writerows(rows)
    print(
        f"Compared {len(rows)} cases; inspect raw sample ranges before concluding a regression.",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
