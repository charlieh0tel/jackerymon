#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyarrow"]
# ///
"""Writes the daemon's sample logs to one Parquet file.

  tools/logs-to-parquet.py OUTPUT [--log-dir DIR] [--unit UNIT ...]
                           [--since TIME] [--until TIME] [--ms]
                           [--names FILE | --serials]

Reads every <serial>/<day>.jsonl and .jsonl.gz under the log directory
(default /var/lib/jackerymon).  A row per sample: unit, timestamp (UTC),
the sample's bookkeeping, then each status field as a column, null where
a sample lacks it.  TIME is ISO 8601; without an offset it is local
time.  --since is inclusive, --until exclusive.  --ms truncates
timestamps to milliseconds, for a smaller file.

The unit column holds names from the names file (default
~/.config/jackerymon/names), so serials stay out of the file; a unit
without a name is refused.  --serials writes serials instead.  UNIT is a
name or a serial.

Lossless otherwise: integers take the narrowest type that holds them,
the columns that climb sample by sample are delta-encoded, and the rest
dictionary-encoded, all under zstd.
"""

import argparse
import gzip
import json
import os
from datetime import datetime
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

DEFAULT_LOG_DIR = Path("/var/lib/jackerymon")
LOG_SUFFIXES = (".jsonl", ".jsonl.gz")
NAMES_FILE = Path("jackerymon/names")
COMMENT = "#"
# Columns that change steadily from one sample to the next, which delta
# encoding stores in a few bits a row; dictionaries suit the others.
DELTA_COLUMNS = ("timestamp", "sample", "connection", "transport_failures")
INTEGER_TYPES = (pa.int8(), pa.int16(), pa.int32(), pa.int64())
COMPRESSION_LEVEL = 19


def default_names() -> Path:
    config = os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config"
    return Path(config) / NAMES_FILE


def load_names(path: Path, required: bool) -> dict[str, str]:
    """Each unit's name by serial, as the CLI reads the names file; none
    when there is no file, unless one is required."""
    try:
        text = path.read_text()
    except FileNotFoundError:
        if required:
            raise SystemExit(f"{path}: no such file")
        return {}
    names = {}
    for number, line in enumerate(text.splitlines(), start=1):
        line = line.strip()
        if not line or line.startswith(COMMENT):
            continue
        fields = line.split(maxsplit=1)
        if len(fields) < 2:
            raise SystemExit(f"{path}: line {number}: no name after {line}")
        serial, name = fields
        if serial in names:
            raise SystemExit(f"{path}: line {number}: unit {serial} named again")
        if name in names.values():
            raise SystemExit(f"{path}: line {number}: name {name!r} given twice")
        names[serial] = name
    return names


def time_arg(text: str) -> datetime:
    time = datetime.fromisoformat(text)
    return time if time.tzinfo else time.astimezone()


def log_files(log_dir: Path, serials: set[str] | None):
    for unit in sorted(path for path in log_dir.iterdir() if path.is_dir()):
        if serials is not None and unit.name not in serials:
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
    parser.add_argument(
        "--unit", action="append", help="name or serial; repeatable; default all"
    )
    parser.add_argument("--since", type=time_arg, help="inclusive")
    parser.add_argument("--until", type=time_arg, help="exclusive")
    parser.add_argument(
        "--ms", action="store_true", help="truncate timestamps to milliseconds"
    )
    labels = parser.add_mutually_exclusive_group()
    labels.add_argument("--names", type=Path, help=f"default {default_names()}")
    labels.add_argument(
        "--serials", action="store_true", help="label rows by serial, not name"
    )
    args = parser.parse_args()

    names_path = args.names or default_names()
    names = load_names(names_path, required=args.names is not None)
    serials_by_name = {name: serial for serial, name in names.items()}
    serials = (
        {serials_by_name.get(unit, unit) for unit in args.unit} if args.unit else None
    )
    if serials is not None:
        unknown = sorted(
            serial for serial in serials if not (args.log_dir / serial).is_dir()
        )
        if unknown:
            parser.exit(
                1,
                f"no unit {' '.join(unknown)}: not a name in {names_path}"
                f" nor a serial in {args.log_dir}\n",
            )

    rows = []
    for serial, path in log_files(args.log_dir, serials):
        for sample in samples(path):
            timestamp = datetime.fromisoformat(sample.pop("timestamp"))
            if args.since and timestamp < args.since:
                continue
            if args.until and timestamp >= args.until:
                continue
            status = sample.pop("status")
            rows.append({"unit": serial, "timestamp": timestamp, **sample, **status})
    if not rows:
        parser.exit(1, "no samples matched\n")

    if not args.serials:
        unnamed = sorted({row["unit"] for row in rows} - names.keys())
        if unnamed:
            parser.exit(
                1,
                f"{len(unnamed)} unit(s) have no name in {names_path}: "
                f"{' '.join(unnamed)}; name them or pass --serials\n",
            )
        for row in rows:
            row["unit"] = names[row["unit"]]

    rows.sort(key=lambda row: (row["unit"], row["timestamp"]))
    # Fields in first-seen order: from_pylist alone takes the first row's.
    columns = list(dict.fromkeys(name for row in rows for name in row))
    table = pa.table(
        {
            column: narrowest(pa.chunked_array([[row.get(column) for row in rows]]))
            for column in columns
        }
    )
    resolution = "ms" if args.ms else "us"
    table = table.set_column(
        table.schema.get_field_index("timestamp"),
        "timestamp",
        table["timestamp"].cast(pa.timestamp(resolution, tz="UTC"), safe=False),
    )
    pq.write_table(
        table,
        args.output,
        compression="zstd",
        compression_level=COMPRESSION_LEVEL,
        use_dictionary=[column for column in columns if column not in DELTA_COLUMNS],
        column_encoding={column: "DELTA_BINARY_PACKED" for column in DELTA_COLUMNS},
    )

    print(f"{args.output}: {table.num_rows} rows, {table.num_columns} columns")
    for label in sorted({row["unit"] for row in rows}):
        times = [row["timestamp"] for row in rows if row["unit"] == label]
        first, last = (
            time.astimezone().isoformat(timespec="seconds")
            for time in (min(times), max(times))
        )
        print(f"{label}: {len(times)} samples, {first} to {last}")


if __name__ == "__main__":
    main()
