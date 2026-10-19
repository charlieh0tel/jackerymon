#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyarrow"]
# ///
"""Writes the daemon's sample logs to one Parquet file.

  tools/logs-to-parquet.py OUTPUT [--log-dir DIR] [--serial SERIAL ...]
                           [--since TIME] [--until TIME]

Reads every <serial>/<day>.jsonl and .jsonl.gz under the log directory
(default /var/lib/jackerymon).  A row per sample: serial, timestamp
(UTC), the sample's bookkeeping, then each status field as a column,
null where a sample lacks it.  TIME is ISO 8601; without an offset it is
local time.  --since is inclusive, --until exclusive.
"""

import argparse
import gzip
import json
from datetime import datetime
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

DEFAULT_LOG_DIR = Path("/var/lib/jackerymon")
LOG_SUFFIXES = (".jsonl", ".jsonl.gz")


def time_arg(text: str) -> datetime:
    time = datetime.fromisoformat(text)
    return time if time.tzinfo else time.astimezone()


def log_files(log_dir: Path, serials: list[str] | None):
    for unit in sorted(path for path in log_dir.iterdir() if path.is_dir()):
        if serials and unit.name not in serials:
            continue
        for path in sorted(unit.iterdir()):
            if path.name.endswith(LOG_SUFFIXES):
                yield unit.name, path


def samples(path: Path):
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt") as lines:
        for line in lines:
            yield json.loads(line)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Write the daemon's sample logs to one Parquet file."
    )
    parser.add_argument("output", type=Path)
    parser.add_argument("--log-dir", type=Path, default=DEFAULT_LOG_DIR)
    parser.add_argument("--serial", action="append", help="repeatable; default all")
    parser.add_argument("--since", type=time_arg, help="inclusive")
    parser.add_argument("--until", type=time_arg, help="exclusive")
    args = parser.parse_args()

    rows = []
    for serial, path in log_files(args.log_dir, args.serial):
        for sample in samples(path):
            timestamp = datetime.fromisoformat(sample.pop("timestamp"))
            if args.since and timestamp < args.since:
                continue
            if args.until and timestamp >= args.until:
                continue
            status = sample.pop("status")
            rows.append({"serial": serial, "timestamp": timestamp, **sample, **status})
    if not rows:
        parser.exit(1, "no samples matched\n")

    rows.sort(key=lambda row: (row["serial"], row["timestamp"]))
    # Fields in first-seen order: from_pylist alone takes the first row's.
    names = list(dict.fromkeys(name for row in rows for name in row))
    table = pa.table({name: [row.get(name) for row in rows] for name in names})
    table = table.set_column(
        table.schema.get_field_index("timestamp"),
        "timestamp",
        table["timestamp"].cast(pa.timestamp("us", tz="UTC")),
    )
    pq.write_table(table, args.output, compression="zstd")

    print(f"{args.output}: {table.num_rows} rows, {table.num_columns} columns")
    for serial in sorted({row["serial"] for row in rows}):
        times = [row["timestamp"] for row in rows if row["serial"] == serial]
        first, last = (time.astimezone().isoformat(timespec="seconds") for time in (min(times), max(times)))
        print(f"{serial}: {len(times)} samples, {first} to {last}")


if __name__ == "__main__":
    main()
