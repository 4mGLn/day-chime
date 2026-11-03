#!/usr/bin/env python3
"""Generate a native messaging manifest; never registers it in a browser."""

import argparse
import json
import os
from pathlib import Path
import re

HOST_NAME = "com.day_chime.attendance"


def manifest(binary: Path, extension_id: str) -> dict:
    if not re.fullmatch(r"[a-p]{32}", extension_id):
        raise ValueError("extension ID must contain exactly 32 letters a-p")
    if not binary.is_absolute():
        raise ValueError("--binary must be an absolute path")
    if not binary.is_file():
        raise ValueError("--binary must point to the built day-chime-browser-host executable")
    if os.name != "nt" and not os.access(binary, os.X_OK):
        raise ValueError("--binary is not executable")
    return {
        "name": HOST_NAME,
        "description": "Day Chime local attendance reader",
        "path": str(binary),
        "type": "stdio",
        "allowed_origins": [f"chrome-extension://{extension_id}/"],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--extension-id", required=True)
    parser.add_argument("--output", type=Path, default=Path.cwd() / "native-host" / f"{HOST_NAME}.json")
    args = parser.parse_args()
    try:
        data = manifest(args.binary, args.extension_id)
    except ValueError as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as output:
        json.dump(data, output, indent=2)
        output.write("\n")
    print(f"Generated {args.output.resolve()}; browser registration has not been performed.")


if __name__ == "__main__":
    main()
