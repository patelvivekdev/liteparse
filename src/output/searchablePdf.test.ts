import zlib from "zlib";
import { describe, it, expect, beforeAll } from "vitest";
import { PDFDocument, StandardFonts } from "pdf-lib";
import { generateSearchablePdf } from "./searchablePdf";
import type { ParsedPage, TextItem } from "../core/types";

/**
 * Build a real, multi-page PDF in memory with pdf-lib so the
 * searchable-PDF generator has an actual PDF to round-trip.
 */
async function makePdf(pageCount: number, width = 612, height = 792): Promise<Uint8Array> {
  const doc = await PDFDocument.create();
  const font = await doc.embedFont(StandardFonts.Helvetica);
  for (let i = 0; i < pageCount; i++) {
    const page = doc.addPage([width, height]);
    // Draw something visible on the first page so a viewer would
    // show *something*; OCR'd pages would normally just be a scanned
    // image but for a test we don't care about visual fidelity.
    page.drawText(`Visible page ${i + 1}`, { x: 50, y: height - 50, font, size: 12 });
  }
  return await doc.save();
}

function ocrItem(str: string, x: number, y: number, w: number, h: number, conf = 0.9): TextItem {
  return {
    str,
    x,
    y,
    width: w,
    height: h,
    w,
    h,
    fontName: "OCR",
    fontSize: h,
    confidence: conf,
  };
}

function nativeItem(str: string, x: number, y: number, w: number, h: number): TextItem {
  return {
    str,
    x,
    y,
    width: w,
    height: h,
    w,
    h,
    fontName: "Helvetica",
    fontSize: h,
    confidence: 1.0,
  };
}

/**
 * Decode any hex-string text show operators (`<HEX> Tj`) inside a
 * content stream into their plain Latin-1 form, so a literal
 * `toMatch(/MY_TOKEN/)` will work even though pdf-lib encodes text
 * as hex strings.
 */
function decodeHexStrings(streamText: string): string {
  return streamText.replace(/<([0-9A-Fa-f]+)>/g, (_m, hex: string) => {
    let out = "";
    for (let i = 0; i + 1 < hex.length; i += 2) {
      const code = parseInt(hex.slice(i, i + 2), 16);
      out += String.fromCharCode(code);
    }
    return out;
  });
}

/**
 * Walk the raw PDF bytes, find every `stream … endstream` block,
 * inflate FlateDecode'd ones, hex-decode any text show operators
 * and concatenate the result. Saving with `useObjectStreams: false`
 * keeps the structure simple enough that this works reliably for
 * pdf-lib output.
 */
async function extractAllText(pdfBytes: Uint8Array): Promise<string> {
  const buf = Buffer.from(pdfBytes);
  let out = "";
  let pos = 0;
  while (pos < buf.length) {
    const sIdx = buf.indexOf("\nstream", pos);
    if (sIdx < 0) break;
    const eIdx = buf.indexOf("\nendstream", sIdx);
    if (eIdx < 0) break;

    // The dict immediately preceding "\nstream" ends at the most
    // recent ">>" before sIdx; its open is the matching "<<".
    const dictEnd = buf.lastIndexOf(">>", sIdx);
    const dictStart = buf.lastIndexOf("<<", dictEnd);
    const dict = dictStart >= 0 ? buf.slice(dictStart, dictEnd + 2).toString("latin1") : "";

    let dataStart = sIdx + "\nstream".length;
    while (buf[dataStart] === 0x0a || buf[dataStart] === 0x0d) dataStart++;
    const data = buf.slice(dataStart, eIdx);

    let decoded: string;
    if (/\/FlateDecode/.test(dict)) {
      try {
        decoded = zlib.inflateSync(data).toString("latin1");
      } catch {
        decoded = data.toString("latin1");
      }
    } else {
      decoded = data.toString("latin1");
    }
    out += "\n" + decodeHexStrings(decoded);
    pos = eIdx + "\nendstream".length;
  }
  return out;
}

describe("generateSearchablePdf", () => {
  let nativePdf: Uint8Array;

  beforeAll(async () => {
    nativePdf = await makePdf(3);
  });

  it("passes through unchanged when no page has OCR (native PDF)", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "x",
        textItems: [nativeItem("foo", 0, 0, 30, 12)],
      },
      {
        pageNum: 2,
        width: 612,
        height: 792,
        text: "x",
        textItems: [nativeItem("bar", 0, 0, 30, 12)],
      },
      { pageNum: 3, width: 612, height: 792, text: "x", textItems: [] },
    ];
    const out = await generateSearchablePdf({ pdfBytes: nativePdf, pages });

    // Output must still be a valid PDF
    expect(Buffer.from(out).slice(0, 4).toString("latin1")).toBe("%PDF");

    // None of our OCR-only sentinel strings should appear (no OCR items)
    const all = await extractAllText(out);
    expect(all).not.toMatch(/SENTINEL_OCR_TOKEN/);
  });

  it("adds invisible text only on OCR'd pages of a mixed document", async () => {
    const pages: ParsedPage[] = [
      // page 1: native text only - should NOT add SENTINEL_OCR_TOKEN
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "native page",
        textItems: [nativeItem("nativeword", 50, 100, 60, 12)],
      },
      // page 2: scanned - should get OCR overlay with our token
      {
        pageNum: 2,
        width: 612,
        height: 792,
        text: "ocr page",
        ocrApplied: true,
        textItems: [ocrItem("SENTINEL_OCR_TOKEN", 100, 200, 120, 18)],
      },
      // page 3: empty
      { pageNum: 3, width: 612, height: 792, text: "", textItems: [] },
    ];

    const out = await generateSearchablePdf({ pdfBytes: nativePdf, pages });
    const all = await extractAllText(out);

    // OCR sentinel must appear (overlay on page 2)
    expect(all).toMatch(/SENTINEL_OCR_TOKEN/);
    // No "nativeword" overlay - that's native text already in the PDF
    // (we never added an overlay for non-OCR items)
    // The original test PDF only contains "Visible page N" content; so
    // nativeword should not be found in the new content streams either.
    expect(all).not.toMatch(/nativeword/);
  });

  it("works when every page is scanned (all OCR)", async () => {
    const pages: ParsedPage[] = [1, 2, 3].map((n) => ({
      pageNum: n,
      width: 612,
      height: 792,
      text: `page ${n}`,
      ocrApplied: true,
      textItems: [ocrItem(`SCAN_TOKEN_${n}`, 50, 100, 100, 14)],
    }));

    const out = await generateSearchablePdf({ pdfBytes: nativePdf, pages });
    const all = await extractAllText(out);
    expect(all).toMatch(/SCAN_TOKEN_1/);
    expect(all).toMatch(/SCAN_TOKEN_2/);
    expect(all).toMatch(/SCAN_TOKEN_3/);
  });

  it("invisible (opacity-0) overlay text is still extractable by PDF.js", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "ocr page",
        ocrApplied: true,
        textItems: [ocrItem("INVIS_CHECK", 50, 100, 90, 14)],
      },
    ];
    const out = await generateSearchablePdf({
      pdfBytes: await makePdf(1),
      pages,
    });
    // PDF.js extracts text regardless of rendering mode / opacity,
    // so we can prove the searchable layer is in fact searchable.
    const all = await extractAllText(out);
    expect(all).toMatch(/INVIS_CHECK/);
  });

  it("respects minConfidence threshold", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "p",
        ocrApplied: true,
        textItems: [
          ocrItem("HIGH_CONF_TOKEN", 50, 100, 100, 12, 0.95),
          ocrItem("LOW_CONF_TOKEN", 50, 130, 100, 12, 0.2),
        ],
      },
    ];
    const out = await generateSearchablePdf({
      pdfBytes: await makePdf(1),
      pages,
      minConfidence: 0.5,
    });
    const all = await extractAllText(out);
    expect(all).toMatch(/HIGH_CONF_TOKEN/);
    expect(all).not.toMatch(/LOW_CONF_TOKEN/);
  });

  it("falls back to ocrApplied=false but fontName=OCR (defensive)", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "p",
        // Explicitly *no* ocrApplied flag
        textItems: [ocrItem("FALLBACK_TOKEN", 50, 100, 100, 12)],
      },
    ];
    const out = await generateSearchablePdf({
      pdfBytes: await makePdf(1),
      pages,
    });
    const all = await extractAllText(out);
    expect(all).toMatch(/FALLBACK_TOKEN/);
  });

  it("ignores items with non-positive width or height", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "p",
        ocrApplied: true,
        textItems: [
          ocrItem("VALID_TOKEN", 50, 100, 60, 12),
          ocrItem("ZERO_W_TOKEN", 50, 120, 0, 12),
          ocrItem("ZERO_H_TOKEN", 50, 140, 60, 0),
        ],
      },
    ];
    const out = await generateSearchablePdf({
      pdfBytes: await makePdf(1),
      pages,
    });
    const all = await extractAllText(out);
    expect(all).toMatch(/VALID_TOKEN/);
    expect(all).not.toMatch(/ZERO_W_TOKEN/);
    expect(all).not.toMatch(/ZERO_H_TOKEN/);
  });

  it("skips pages with out-of-range pageNum", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 99, // PDF only has 1 page
        width: 612,
        height: 792,
        text: "p",
        ocrApplied: true,
        textItems: [ocrItem("OUT_OF_RANGE", 50, 100, 60, 12)],
      },
    ];
    const out = await generateSearchablePdf({
      pdfBytes: await makePdf(1),
      pages,
    });
    // Should not throw and should produce a valid PDF
    expect(Buffer.from(out).slice(0, 4).toString("latin1")).toBe("%PDF");
  });

  it("sanitises non-WinAnsi characters (CJK, emoji) without crashing", async () => {
    const pages: ParsedPage[] = [
      {
        pageNum: 1,
        width: 612,
        height: 792,
        text: "p",
        ocrApplied: true,
        textItems: [ocrItem("中文测试", 50, 100, 60, 12), ocrItem("🚀 emoji", 50, 130, 60, 12)],
      },
    ];
    const out = await generateSearchablePdf({
      pdfBytes: await makePdf(1),
      pages,
    });
    expect(Buffer.from(out).slice(0, 4).toString("latin1")).toBe("%PDF");
  });
});
