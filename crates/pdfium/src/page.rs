use std::marker::PhantomData;

use crate::bitmap::Bitmap;
use crate::document::Document;
use crate::error::PdfiumError;
use crate::ffi;
use crate::text_page::TextPage;
use crate::types::RectF;

/// Bounding box of an embedded image object on a page.
/// Coordinates are in PDF points with top-left origin (Y-down).
#[derive(Debug, Clone, Copy)]
pub struct ImageBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A loaded page within a [`Document`].
///
/// The `'doc` lifetime ties the page to its owning document; `'lib` carries
/// the PDFium-lock lifetime through, ensuring no PDFium calls can occur
/// after the lock is released.
pub struct Page<'doc, 'lib: 'doc> {
    pub(crate) handle: pdfium_sys::FPDF_PAGE,
    pub(crate) doc_handle: pdfium_sys::FPDF_DOCUMENT,
    pub(crate) _doc: PhantomData<&'doc Document<'lib>>,
}

impl<'doc, 'lib: 'doc> Page<'doc, 'lib> {
    pub fn width(&self) -> f32 {
        unsafe { ffi!(FPDF_GetPageWidthF(self.handle)) }
    }

    pub fn height(&self) -> f32 {
        unsafe { ffi!(FPDF_GetPageHeightF(self.handle)) }
    }

    pub fn rotation(&self) -> i32 {
        unsafe { ffi!(FPDFPage_GetRotation(self.handle)) }
    }

    /// Get the page bounding box (CropBox, falls back to MediaBox).
    /// Coordinates in PDF page space.
    pub fn view_box(&self) -> Option<RectF> {
        let mut rect = pdfium_sys::FS_RECTF {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        let ok = unsafe { ffi!(FPDF_GetPageBoundingBox(self.handle, &mut rect)) };
        if ok != 0 {
            Some(RectF {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            })
        } else {
            None
        }
    }

    /// Convert a point from PDF page space to viewport space (top-left origin, 72 DPI).
    /// Mirrors the platform's Parse_pageToViewport using FPDF_PageToDevice at 1000x scale.
    pub fn page_to_viewport(&self, view_box: &RectF, page_x: f32, page_y: f32) -> (f32, f32) {
        let mut vw = view_box.right - view_box.left;
        let mut vh = view_box.top - view_box.bottom;

        let rotation = self.rotation();
        if rotation == 1 || rotation == 3 {
            // 90° or 270° — swap viewport dimensions
            std::mem::swap(&mut vw, &mut vh);
        }

        let device_w = (vw * 1000.0).round() as i32;
        let device_h = (vh * 1000.0).round() as i32;
        let mut dx: i32 = 0;
        let mut dy: i32 = 0;

        unsafe {
            ffi!(FPDF_PageToDevice(
                self.handle,
                0,
                0,
                device_w,
                device_h,
                0, // rotation 0 — PDFium applies page rotation internally
                page_x as f64,
                page_y as f64,
                &mut dx,
                &mut dy,
            ));
        }

        (dx as f32 / 1000.0, dy as f32 / 1000.0)
    }

    /// Convert bounds from PDF page space to viewport space (top-left origin).
    /// Returns RectF with left/top/right/bottom in viewport coordinates.
    pub fn bounds_to_viewport(&self, view_box: &RectF, page_bounds: &RectF) -> RectF {
        let (ll_x, ll_y) = self.page_to_viewport(view_box, page_bounds.left, page_bounds.bottom);
        let (ur_x, ur_y) = self.page_to_viewport(view_box, page_bounds.right, page_bounds.top);

        RectF {
            left: ll_x.min(ur_x),
            top: ll_y.min(ur_y),
            right: ll_x.max(ur_x),
            bottom: ll_y.max(ur_y),
        }
    }

    pub fn text(&self) -> Result<TextPage<'_, 'lib>, PdfiumError> {
        let handle = unsafe { ffi!(FPDFText_LoadPage(self.handle)) };
        if handle.is_null() {
            return Err(PdfiumError::OperationFailed);
        }
        Ok(TextPage {
            handle,
            _page: PhantomData,
        })
    }

    /// Render the page to a BGRA bitmap at the given DPI.
    pub fn render(&self, dpi: f32) -> Result<Bitmap<'lib>, PdfiumError> {
        let scale = dpi / 72.0;
        let width = (self.width() * scale).round() as i32;
        let height = (self.height() * scale).round() as i32;

        // SAFETY: this method is on `Page<'_, 'lib>`, whose existence proves
        // the PDFium lock is held for `'lib`; the returned `Bitmap<'lib>` is
        // tied to that same lock lifetime.
        let bitmap = unsafe { Bitmap::new(width, height) }?;

        // Fill with white (ARGB: 0xFFFFFFFF)
        bitmap.fill_rect(0, 0, width, height, 0xFFFFFFFF);

        let flags = (pdfium_sys::FPDF_ANNOT | pdfium_sys::FPDF_PRINTING) as i32;

        unsafe {
            ffi!(FPDF_RenderPageBitmap(
                bitmap.handle(),
                self.handle,
                0,      // start_x
                0,      // start_y
                width,  // size_x
                height, // size_y
                0,      // rotation
                flags,
            ));
        }

        Ok(bitmap)
    }

    /// Extract bounding boxes of embedded image objects on this page.
    /// Returns coordinates in viewport space (Y-down, top-left origin) in PDF points.
    /// Filters out images smaller than `min_size_pt` and images covering more than
    /// `max_page_coverage` fraction of the page.
    pub fn image_bounds(&self, min_size_pt: f32, max_page_coverage: f32) -> Vec<ImageBounds> {
        let page_width = self.width();
        let page_height = self.height();
        let obj_count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        let mut results = Vec::new();

        for i in 0..obj_count {
            let obj = unsafe { ffi!(FPDFPage_GetObject(self.handle, i)) };
            if obj.is_null() {
                continue;
            }

            let obj_type = unsafe { ffi!(FPDFPageObj_GetType(obj)) };
            if obj_type != pdfium_sys::FPDF_PAGEOBJ_IMAGE as i32 {
                continue;
            }

            let mut left: f32 = 0.0;
            let mut bottom: f32 = 0.0;
            let mut right: f32 = 0.0;
            let mut top: f32 = 0.0;
            let ok = unsafe {
                ffi!(FPDFPageObj_GetBounds(
                    obj,
                    &mut left,
                    &mut bottom,
                    &mut right,
                    &mut top
                ))
            };
            if ok == 0 {
                continue;
            }

            let w = right - left;
            let h = top - bottom;

            if w < min_size_pt || h < min_size_pt {
                continue;
            }
            if w > page_width * max_page_coverage && h > page_height * max_page_coverage {
                continue;
            }

            // Convert from PDF coords (bottom-left origin) to viewport (top-left origin)
            results.push(ImageBounds {
                x: left,
                y: page_height - top,
                width: w,
                height: h,
            });
        }

        results
    }

    /// Extract bounding boxes of path objects that look like vector glyph
    /// outlines (text flattened to filled Bézier paths, carrying no character
    /// codes). Such content renders as perfectly readable text but is
    /// invisible to text extraction, so callers can use these bounds to
    /// decide whether a page needs OCR despite having a dense text layer.
    ///
    /// Heuristic per path object:
    /// - fill mode is set (glyph outlines are filled; chart lines, table rules
    ///   and borders are stroked),
    /// - Bézier-heavy (letterforms are mostly curves; rules/borders are pure
    ///   move/line segments, rounded rects have only 4 curves),
    /// - bounds are text-sized (excludes full-page background art).
    ///
    /// Form XObjects are recursed into (depth-limited) because flattened text
    /// is frequently wrapped in one.
    ///
    /// Returns coordinates in viewport space (Y-down, top-left origin) in PDF
    /// points, like [`Self::image_bounds`].
    pub fn glyph_like_path_bounds(&self) -> Vec<ImageBounds> {
        let page_height = self.height();
        let obj_count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        let mut results = Vec::new();

        for i in 0..obj_count {
            let obj = unsafe { ffi!(FPDFPage_GetObject(self.handle, i)) };
            if obj.is_null() {
                continue;
            }
            collect_glyph_like_paths(obj, page_height, 0, &mut results);
        }

        results
    }

    /// Get the rendered bitmap of a specific embedded image object by index.
    /// The index corresponds to the order from iterating page objects (image objects only).
    pub fn render_image_object(&self, image_obj_index: usize) -> Result<Bitmap<'lib>, PdfiumError> {
        let obj_count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        let mut image_idx = 0usize;

        for i in 0..obj_count {
            let obj = unsafe { ffi!(FPDFPage_GetObject(self.handle, i)) };
            if obj.is_null() {
                continue;
            }
            let obj_type = unsafe { ffi!(FPDFPageObj_GetType(obj)) };
            if obj_type != pdfium_sys::FPDF_PAGEOBJ_IMAGE as i32 {
                continue;
            }

            if image_idx == image_obj_index {
                let bmp_handle = unsafe {
                    ffi!(FPDFImageObj_GetRenderedBitmap(
                        self.doc_handle,
                        self.handle,
                        obj
                    ))
                };
                if bmp_handle.is_null() {
                    return Err(PdfiumError::OperationFailed);
                }
                // Wrap in our Bitmap (which will call Destroy on drop)
                return Ok(unsafe { Bitmap::from_handle(bmp_handle) });
            }
            image_idx += 1;
        }

        Err(PdfiumError::OperationFailed)
    }
}

/// Walk a page object (recursing into Form XObjects) and collect bounds of
/// glyph-outline-like path objects. See [`Page::glyph_like_path_bounds`].
fn collect_glyph_like_paths(
    obj: pdfium_sys::FPDF_PAGEOBJECT,
    page_height: f32,
    depth: usize,
    results: &mut Vec<ImageBounds>,
) {
    // Flattened text is usually at the top level or one Form XObject deep;
    // a small cap keeps malicious/degenerate PDFs from causing deep recursion.
    const MAX_FORM_DEPTH: usize = 2;
    // Letterforms are predominantly Bézier curves. A rounded rectangle has 4;
    // even a single lowercase glyph outline typically carries 6+.
    const MIN_BEZIER_SEGMENTS: usize = 6;
    const MIN_TOTAL_SEGMENTS: usize = 10;
    // Some producers flatten the curves themselves, emitting glyph outlines as
    // hundreds of tiny line segments with zero Béziers. Those paths are still
    // recognizable by their extreme segment *density*: a line-flattened text
    // run carries ~0.3-0.5 segments per pt² of its bounds, while filled
    // decorations (table rules, shaded rows, sawtooth borders) sit well under
    // ~0.05. Both thresholds must hold so small busy specks don't qualify.
    const MIN_FLATTENED_SEGMENTS: usize = 64;
    const MIN_SEGMENT_DENSITY: f32 = 0.15; // segments per pt² of bbox
    // Text-sized bounds: at least a couple of points each way (skips specks
    // and hairline rules), no taller than a stacked paragraph block.
    const MIN_DIM_PT: f32 = 2.0;
    const MAX_HEIGHT_PT: f32 = 120.0;

    let obj_type = unsafe { ffi!(FPDFPageObj_GetType(obj)) };

    if obj_type == pdfium_sys::FPDF_PAGEOBJ_FORM as i32 {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let count = unsafe { ffi!(FPDFFormObj_CountObjects(obj)) };
        for i in 0..count {
            let child = unsafe { ffi!(FPDFFormObj_GetObject(obj, i as std::os::raw::c_ulong)) };
            if child.is_null() {
                continue;
            }
            collect_glyph_like_paths(child, page_height, depth + 1, results);
        }
        return;
    }

    if obj_type != pdfium_sys::FPDF_PAGEOBJ_PATH as i32 {
        return;
    }

    // Must be filled: glyph outlines are filled shapes. Stroked-only paths
    // (table rules, underlines, chart lines, borders) are never letterforms.
    let mut fillmode: std::os::raw::c_int = 0;
    let mut stroke: pdfium_sys::FPDF_BOOL = 0;
    let ok = unsafe { ffi!(FPDFPath_GetDrawMode(obj, &mut fillmode, &mut stroke)) };
    if ok == 0 || fillmode == pdfium_sys::FPDF_FILLMODE_NONE as i32 {
        return;
    }

    let seg_count = unsafe { ffi!(FPDFPath_CountSegments(obj)) };
    if seg_count < MIN_TOTAL_SEGMENTS as i32 {
        return;
    }
    let mut beziers = 0usize;
    for i in 0..seg_count {
        let seg = unsafe { ffi!(FPDFPath_GetPathSegment(obj, i)) };
        if seg.is_null() {
            continue;
        }
        let seg_type = unsafe { ffi!(FPDFPathSegment_GetType(seg)) };
        if seg_type == pdfium_sys::FPDF_SEGMENT_BEZIERTO as i32 {
            beziers += 1;
        }
    }

    let mut left: f32 = 0.0;
    let mut bottom: f32 = 0.0;
    let mut right: f32 = 0.0;
    let mut top: f32 = 0.0;
    let ok = unsafe { ffi!(FPDFPageObj_GetBounds(obj, &mut left, &mut bottom, &mut right, &mut top)) };
    if ok == 0 {
        return;
    }

    let w = right - left;
    let h = top - bottom;
    if w < MIN_DIM_PT || h < MIN_DIM_PT || h > MAX_HEIGHT_PT {
        return;
    }

    // Two glyph-outline profiles:
    // (a) curve-preserving producers: Bézier-heavy paths;
    // (b) curve-flattening producers: zero Béziers, but an extreme density of
    //     tiny line segments packed into a text-sized box.
    let area = w * h;
    let bezier_like = beziers >= MIN_BEZIER_SEGMENTS;
    let flattened_like = seg_count as usize >= MIN_FLATTENED_SEGMENTS
        && area > 0.0
        && seg_count as f32 / area >= MIN_SEGMENT_DENSITY;
    if !bezier_like && !flattened_like {
        return;
    }

    // Convert from PDF coords (bottom-left origin) to viewport (top-left origin).
    results.push(ImageBounds {
        x: left,
        y: page_height - top,
        width: w,
        height: h,
    });
}

/// Pre-computed affine transform from PDF page space to viewport space.
/// Avoids repeated FFI calls to `FPDF_PageToDevice` by probing 3 points
/// once and deriving the 6 affine coefficients.
#[derive(Debug, Clone, Copy)]
pub struct ViewportTransform {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl ViewportTransform {
    /// Transform a single point from page space to viewport space.
    #[inline]
    pub fn transform_point(&self, page_x: f32, page_y: f32) -> (f32, f32) {
        (
            self.a * page_x + self.b * page_y + self.e,
            self.c * page_x + self.d * page_y + self.f,
        )
    }

    /// Transform a bounding rect from page space to viewport space.
    #[inline]
    pub fn transform_bounds(&self, page_bounds: &RectF) -> RectF {
        let (ll_x, ll_y) = self.transform_point(page_bounds.left, page_bounds.bottom);
        let (ur_x, ur_y) = self.transform_point(page_bounds.right, page_bounds.top);
        RectF {
            left: ll_x.min(ur_x),
            top: ll_y.min(ur_y),
            right: ll_x.max(ur_x),
            bottom: ll_y.max(ur_y),
        }
    }
}

impl<'doc, 'lib: 'doc> Page<'doc, 'lib> {
    /// Build a `ViewportTransform` by probing 3 points through PDFium.
    /// This makes 3 FFI calls total, after which all transforms are pure math.
    pub fn viewport_transform(&self, view_box: &RectF) -> ViewportTransform {
        let (e, f) = self.page_to_viewport(view_box, 0.0, 0.0);
        let (ax_e, cx_f) = self.page_to_viewport(view_box, 1.0, 0.0);
        let (by_e, dy_f) = self.page_to_viewport(view_box, 0.0, 1.0);

        ViewportTransform {
            a: ax_e - e,
            b: by_e - e,
            c: cx_f - f,
            d: dy_f - f,
            e,
            f,
        }
    }
}

impl Drop for Page<'_, '_> {
    fn drop(&mut self) {
        unsafe { ffi!(FPDF_ClosePage(self.handle)) };
    }
}
