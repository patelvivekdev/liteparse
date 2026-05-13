import { PDFDocument, PDFFont, PDFPage, StandardFonts, rgb } from "pdf-lib";
import { ParsedPage, TextItem } from "../core/types.js";

/**
 * Options accepted by {@link generateSearchablePdf}.
 */
export interface GenerateSearchablePdfOptions {
  /**
   * Bytes of the source PDF. Pages from this PDF are preserved
   * exactly; an invisible OCR text layer is added on top of pages
   * that were OCR'd.
   */
  pdfBytes: Uint8Array;

  /**
   * Parsed pages produced by {@link LiteParse.parse}. Pages whose
   * `ocrApplied === true` (or that contain `fontName === "OCR"` text
   * items) get an invisible text layer; other pages are passed
   * through unchanged so a mixed document keeps its native text
   * search intact.
   */
  pages: ParsedPage[];

  /**
   * Minimum OCR confidence (0-1) for a text item to be added to the
   * invisible layer. Defaults to `0` (include everything LiteParse
   * already kept after its own filtering).
   */
  minConfidence?: number;
}

const DEFAULT_MIN_CONFIDENCE = 0;

/**
 * Filter to OCR-sourced text items only. We use `fontName === "OCR"`
 * because LiteParse tags every OCR-derived item with that synthetic
 * font name (see parser.ts and bbox.ts).
 */
function getOcrItems(page: ParsedPage, minConfidence: number): TextItem[] {
  return page.textItems.filter((it) => {
    if (it.fontName !== "OCR") return false;
    if (!it.str || it.str.trim().length === 0) return false;
    const w = it.width || it.w || 0;
    const h = it.height || it.h || 0;
    if (w <= 0 || h <= 0) return false;
    const conf = it.confidence ?? 1;
    return conf >= minConfidence;
  });
}

/**
 * True if this page needs an invisible OCR overlay.
 */
function pageNeedsOverlay(page: ParsedPage): boolean {
  if (page.ocrApplied) return true;
  // Defensive fallback: if any OCR-flagged item exists even without
  // the explicit ocrApplied flag, still draw the layer.
  return page.textItems.some((it) => it.fontName === "OCR");
}

/**
 * Replace any character outside the WinAnsi (Helvetica) encoding
 * with `?`. The invisible text only needs to be searchable - the
 * exact character is irrelevant for visual fidelity. This keeps
 * the implementation dependency-light (no need for a CJK font).
 */
function sanitizeForWinAnsi(str: string): string {
  let out = "";
  for (const ch of str) {
    const code = ch.charCodeAt(0);
    // Printable Basic Latin / Latin-1 Supplement
    if ((code >= 0x20 && code <= 0x7e) || (code >= 0xa0 && code <= 0xff)) {
      out += ch;
    } else {
      out += "?";
    }
  }
  return out;
}

/**
 * Draw one OCR text item as invisible text on top of `page`.
 *
 * Strategy: scale the font so the rendered string width equals the
 * OCR bounding-box width, then draw with `opacity: 0` (alpha 0) so
 * the text exists in the content stream and is selectable / indexable
 * but never actually painted. PDF coordinates are bottom-left origin,
 * LiteParse coordinates are top-left, so we flip Y.
 */
function drawInvisibleTextItem(
  page: PDFPage,
  font: PDFFont,
  item: TextItem,
  pageHeight: number
): void {
  const text = sanitizeForWinAnsi(item.str);
  if (!text) return;

  const w = item.width || item.w || 0;
  const h = item.height || item.h || 0;
  if (w <= 0 || h <= 0) return;

  // Width-of-text at 1pt; if zero (e.g. all unrenderable chars stripped)
  // fall back to box height as font size.
  let widthAt1pt: number;
  try {
    widthAt1pt = font.widthOfTextAtSize(text, 1);
  } catch {
    return;
  }
  if (!isFinite(widthAt1pt) || widthAt1pt <= 0) {
    return;
  }

  let fontSize = w / widthAt1pt;
  // Cap to bbox height-ish so text never balloons absurdly when the
  // OCR bbox is wide-but-short (single tall glyph).
  const maxByHeight = Math.max(h * 1.2, 1);
  if (fontSize > maxByHeight) fontSize = maxByHeight;
  if (!isFinite(fontSize) || fontSize <= 0) return;

  // Convert top-left LiteParse coords → bottom-left PDF coords.
  // Place the baseline at (item.y + h) inside the bbox.
  const x = item.x;
  const baselineY = pageHeight - (item.y + h);

  page.drawText(text, {
    x,
    y: baselineY,
    size: fontSize,
    font,
    color: rgb(0, 0, 0),
    opacity: 0, // truly invisible but selectable / searchable
  });
}

/**
 * Generate a searchable PDF by overlaying invisible OCR text on
 * the OCR'd pages of the source PDF. Pages with native text are
 * left untouched, so:
 *
 * - **Native PDFs** → pass through, already searchable.
 * - **Scanned PDFs** → every page gets an OCR overlay.
 * - **Mixed PDFs** → only the OCR'd pages get an overlay.
 *
 * Returns the bytes of the modified PDF.
 */
export async function generateSearchablePdf(
  opts: GenerateSearchablePdfOptions
): Promise<Uint8Array> {
  const minConfidence = opts.minConfidence ?? DEFAULT_MIN_CONFIDENCE;

  const pdfDoc = await PDFDocument.load(opts.pdfBytes, {
    // Keep extra metadata so the round-tripped PDF is as close as
    // possible to the original.
    updateMetadata: false,
  });
  const font = await pdfDoc.embedFont(StandardFonts.Helvetica);
  const pdfPages = pdfDoc.getPages();

  for (const parsedPage of opts.pages) {
    if (!pageNeedsOverlay(parsedPage)) continue;

    const pageIdx = parsedPage.pageNum - 1;
    if (pageIdx < 0 || pageIdx >= pdfPages.length) continue;

    const ocrItems = getOcrItems(parsedPage, minConfidence);
    if (ocrItems.length === 0) continue;

    const pdfPage = pdfPages[pageIdx];
    const { height: pageHeight } = pdfPage.getSize();

    for (const item of ocrItems) {
      drawInvisibleTextItem(pdfPage, font, item, pageHeight);
    }
  }

  // `useObjectStreams: false` keeps every PDF object as a top-level
  // indirect object instead of bundling them into a compressed
  // object-stream. Output is slightly larger but the structure is
  // friendlier for downstream PDF tooling, debugging, and avoids
  // gratuitous re-compression of unchanged objects.
  return await pdfDoc.save({ useObjectStreams: false });
}
