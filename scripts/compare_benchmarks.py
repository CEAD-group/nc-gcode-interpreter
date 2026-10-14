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
        "verify_digest",
    ):
        if baseline["arguments"].get(field) != candidate["arguments"].get(field):
            parser.error(f"incompatible benchmark setting: {field}")
    previous = {
        tuple(row[key] for key in CASE_FIELDS): row
        for row in baseline["results"]
        if row["exit_code"] == 0
    }
    writer = csv.writer(sys.stdout)
    writer.writerow(
        (*CASE_FIELDS, "baseline_ms", "candidate_ms", "time_delta_pct", "rss_delta_pct")
    )
    matched = 0
    for row in candidate["results"]:
        key = tuple(row[key] for key in CASE_FIELDS)
        if key not in previous or row["exit_code"] != 0:
            continue
        old = previous[key]
        if {s["rows"] for s in old["samples"]} != {s["rows"] for s in row["samples"]}:
            parser.error(
                f"output row counts changed for {key}; check correctness first"
            )
        writer.writerow(
            (
                *key,
                round(old["warm_median_ms"], 3),
                round(row["warm_median_ms"], 3),
                round(100 * (row["warm_median_ms"] / old["warm_median_ms"] - 1), 2),
                round(100 * (row["peak_rss_bytes"] / old["peak_rss_bytes"] - 1), 2),
            )
        )
        matched += 1
    if not matched:
        parser.error("no successful matching cases")
    print(
        f"Compared {matched} cases; inspect raw sample ranges before concluding a regression.",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
