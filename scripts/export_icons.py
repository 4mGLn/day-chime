#!/usr/bin/env python3
"""Export application/extension PNG sizes from the generated master (requires Pillow)."""
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SIZES = (16, 32, 48, 64, 128, 256, 512)


def main() -> None:
    assets = ROOT / 'assets'
    extension = ROOT / 'extension' / 'icons'
    extension.mkdir(parents=True, exist_ok=True)
    with Image.open(assets / 'day-chime-icon.png') as source:
        image = source.convert('RGBA')
        for size in SIZES:
            exported = image.resize((size, size), Image.Resampling.LANCZOS)
            exported.save(assets / f'day-chime-{size}.png', optimize=True)
            if size in (16, 32, 48, 128):
                exported.save(extension / f'icon-{size}.png', optimize=True)


if __name__ == '__main__':
    main()
