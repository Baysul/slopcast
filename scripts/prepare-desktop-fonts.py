"""Build static font collections for GPUI from the licensed variable fonts.

Run: uv run --with fonttools python scripts/prepare-desktop-fonts.py
GPUI 0.9 selects faces by weight but does not instantiate variable font axes.
"""
from pathlib import Path

from fontTools.ttLib import TTCollection, TTFont
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parent.parent / "apps/desktop/resources/fonts"
WEIGHTS = [(400, "Regular"), (500, "Medium"), (600, "SemiBold"), (700, "Bold")]
for family in ("Inter", "Lora", "JetBrainsMono"):
    collection = TTCollection()
    collection.fonts = []
    for weight, style in WEIGHTS:
        font = instantiateVariableFont(TTFont(ROOT / f"{family}.ttf"), {"wght": weight}, inplace=True)
        font["OS/2"].usWeightClass = weight
        # Give font databases a distinct face name as well as the correct weight.
        for record in font["name"].names:
            if record.nameID in (2, 17):
                record.string = style.encode(record.getEncoding())
            elif record.nameID == 6:
                record.string = f"{family}-{style}".encode(record.getEncoding())
        collection.fonts.append(font)
    collection.save(ROOT / f"{family}.ttc", shareTables=True)
