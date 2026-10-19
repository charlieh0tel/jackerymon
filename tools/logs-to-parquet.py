#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyarrow"]
# ///
"""Writes the daemon's sample logs to one Parquet file.

  tools/logs-to-parquet.py OUTPUT [--log-dir DIR] [--serial SERIAL ...]
                           [--since TIME] [--until TIME] [--ms]

Reads every <serial>/<day>.jsonl and .jsonl.gz under the log directory
(default /var/lib/jackerymon).  A row per sample: serial, timestamp
(UTC), the sample's bookkeeping, then each status field as a column,
null where a sample lacks it.  TIME is ISO 8601; without an offset it is
local time.  --since is inclusive, --until exclusive.  --ms truncates
timestamps to milliseconds, for a smaller file.

Lossless otherwise: integers take the narrowest type that holds them,
the columns that climb sample by sample are delta-encoded, and the rest
dictionary-encoded, all under zstd.
"""

import argparse
import gzip
import json
from datetime import datetime
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

DEFAULT_LOG_DIR = Path("/var/lib/jackerymon")
LOG_SUFFIXES = (".jsonl", ".jsonl.gz")
# Columns that change steadily from one sample to the next, which delta
# encoding stores in a few bits a row; dictionaries suit the others.
DELTA_COLUMNS = ("timestamp", "sample", "connection", "transport_failures")
INTEGER_TYPES = (pa.int8(), pa.int16(), pa.int32(), pa.int64())
COMPRESSION_LEVEL = 19


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


def narrowest(column: pa.ChunkedArray) -> pa.ChunkedArray:
    if not pa.types.is_integer(column.type):
        return column
    bounds = pc.min_max(column)
    low, high = bounds["min"].as_py(), bounds["max"].as_py()
    for integer in INTEGER_TYPES:
        limit = 1 << (integer.bit_width - 1)
        if -limit <= low and high < limit:
            return column.cast(integer)
    return column


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Write the daemon's sample logs to one Parquet file."
    )
    parser.add_argument("output", type=Path)
    parser.add_argument("--log-dir", type=Path, default=DEFAULT_LOG_DIR)
    parser.add_argument("--serial", action="append", help="repeatable; default all")
    parser.add_argument("--since", type=time_arg, help="inclusive")
    parser.add_argument("--until", type=time_arg, help="exclusive")
    parser.add_argument(
        "--ms", action="store_true", help="truncate timestamps to milliseconds"
    )
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
    table = pa.table(
        {
            name: narrowest(pa.chunked_array([[row.get(name) for row in rows]]))
            for name in names
        }
    )
    unit = "ms" if args.ms else "us"
    table = table.set_column(
        table.schema.get_field_index("timestamp"),
        "timestamp",
        table["timestamp"].cast(pa.timestamp(unit, tz="UTC"), safe=False),
    )
    pq.write_table(
        table,
        args.output,
        compression="zstd",
        compression_level=COMPRESSION_LEVEL,
        use_dictionary=[name for name in names if name not in DELTA_COLUMNS],
        column_encoding={name: "DELTA_BINARY_PACKED" for name in DELTA_COLUMNS},
    )

    print(f"{args.output}: {table.num_rows} rows, {table.num_columns} columns")
    for serial in sorted({row["serial"] for row in rows}):
        times = [row["timestamp"] for row in rows if row["serial"] == serial]
        first, last = (
            time.astimezone().isoformat(timespec="seconds")
            for time in (min(times), max(times))
        )
        print(f"{serial}: {len(times)} samples, {first} to {last}")


if __name__ == "__main__":
    main()
