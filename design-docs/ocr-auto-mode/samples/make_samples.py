"""Generate sample PDFs that exercise each native-text corruption class.

Run with the ocr-demo venv (has PyMuPDF/fitz):
    ../../../../liteparse-ocr-demo/.venv/bin/python make_samples.py

Probe a result with the Rust internals probe:
    cd ../../../ && cargo run --example probe_internals -- \
        design-docs/ocr-auto-mode/samples/<file>.pdf 1 chars

Classes (see PRD):
  1  broken encoding  -> PUA / control codepoints   (is_buggy_codepoint fires)
  2  subset font bug  -> TT.../+TT name, Type1 ABCDEF_  (is_buggy_font; needs fonttools)
  3  valid-but-wrong  -> Caesar/cipher mapping       (NO existing flag fires)
"""

from __future__ import annotations

from pathlib import Path

import fitz

HERE = Path(__file__).resolve().parent
# An embedded TrueType font is REQUIRED for is_buggy_codepoint() to fire:
# extract.rs only checks codepoints when font_is_embedded is true. Base-14
# fonts (helv) are never embedded, so they never trip font_is_buggy.
EMBED_TTF = "/snap/storage-explorer/78/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"


def clean(path: Path) -> None:
    """Baseline: correct native text, normal font. No flag should fire."""
    doc = fitz.open()
    page = doc.new_page(width=612, height=792)
    page.insert_text((54, 60), "INVOICE TRACKING FORM", fontname="helv", fontsize=12)
    page.insert_text((54, 80), "Line  Description     Price   Total", fontname="helv", fontsize=11)
    page.insert_text((54, 100), "1  Conceptual Design   23,750.00", fontname="helv", fontsize=11)
    doc.save(path)
    doc.close()


def pua(path: Path) -> None:
    """Class 1: native text mapped into the Unicode Private Use Area.

    Mimics a broken ToUnicode CMap whose glyphs resolve to U+E0xx. The visible
    glyphs (what OCR would read) are the real words; the *extracted* codepoints
    are PUA, which is exactly what is_buggy_codepoint() flags.
    """
    doc = fitz.open()
    page = doc.new_page(width=612, height=792)
    # Good native line (clean) + a "corrupt" line whose codepoints are PUA.
    # The corrupt line uses an EMBEDDED font so is_buggy_codepoint() applies.
    page.insert_text((54, 60), "INVOICE TRACKING FORM", fontname="helv", fontsize=12)
    corrupt = "".join(chr(0xE000 + (ord(c) & 0xFF)) for c in "Line Description Price Total")
    page.insert_text((54, 90), corrupt, fontname="DV", fontfile=EMBED_TTF, fontsize=11)
    doc.save(path)
    doc.close()


def control(path: Path) -> None:
    """Class 1b: codepoints <= U+001F (control range)."""
    doc = fitz.open()
    page = doc.new_page(width=612, height=792)
    # Non-embedded font: control codepoints survive extraction as items AND each
    # char reports has_unicode_map_error (fitz won't write a ToUnicode for them),
    # so the items come out has_map_error=true while staying present to block OCR.
    page.insert_text((54, 60), "INVOICE TRACKING FORM", fontname="helv", fontsize=12)
    corrupt = "".join(chr(1 + (ord(c) % 0x1E)) for c in "Line Description Price Total")
    page.insert_text((54, 90), corrupt, fontname="helv", fontsize=11)
    doc.save(path)
    doc.close()


def main() -> None:
    clean(HERE / "cond_clean.pdf")
    pua(HERE / "cond_pua.pdf")
    control(HERE / "cond_control.pdf")
    print("wrote:", *(p.name for p in sorted(HERE.glob("cond_*.pdf"))))


if __name__ == "__main__":
    main()
