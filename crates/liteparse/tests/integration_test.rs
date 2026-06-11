use std::path::Path;

use liteparse::conversion::convert_data_to_pdf;
use liteparse::types::PdfInput;
use liteparse::{LiteParse, LiteParseConfig};
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_screenshot_image_integration() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let lit = LiteParse::new(LiteParseConfig::default());
    let results = lit
        .screenshot("../../integration_tests_data/receipt.png", None)
        .await
        .expect("Should be able to screenshot converted image");
    assert_eq!(results.len(), 1);
    assert!(results[0].width > 0);
    assert!(results[0].height > 0);
    assert!(!results[0].image_bytes.is_empty());
}

#[tokio::test]
#[serial]
async fn test_screenshot_pdf_integration() {
    let lit = LiteParse::new(LiteParseConfig::default());
    let results = lit
        .screenshot("../../integration_tests_data/sample.pdf", None)
        .await
        .expect("Should be able to screenshot PDF");
    assert_eq!(results.len(), 1);
    assert!(!results[0].image_bytes.is_empty());
}

#[tokio::test]
async fn test_screenshot_rejects_text_file() {
    let dir = tempfile::tempdir().unwrap();
    let txt_path = dir.path().join("notes.txt");
    std::fs::write(&txt_path, "hello").unwrap();
    let lit = LiteParse::new(LiteParseConfig::default());
    let err = lit
        .screenshot(txt_path.to_str().unwrap(), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Cannot screenshot text-based format"));
}

#[tokio::test]
#[serial]
async fn test_convert_data_to_pdf_integration() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let fixture_path = "../../integration_tests_data/receipt.png";
    let data = tokio::fs::read(fixture_path)
        .await
        .expect("Should be able to read file");
    let (converted, _temps) = convert_data_to_pdf(data, None)
        .await
        .expect("Should be able to convert data to PDF");
    assert!(Path::new(&converted.pdf_path).exists());
}

#[tokio::test]
#[serial]
async fn test_parse_bytes_image_integration() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let fixture_path = "../../integration_tests_data/receipt.png";
    let lit = LiteParse::new(LiteParseConfig::default());
    let data = tokio::fs::read(fixture_path)
        .await
        .expect("Should be able to read file");
    let input = PdfInput::Bytes(data);
    let parsed = lit
        .parse_input(input)
        .await
        .expect("Should be able to parse");
    assert_eq!(parsed.pages.len(), 1);
}

#[tokio::test]
#[serial]
async fn test_parse_bytes_office_integration() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let fixture_path = "../../integration_tests_data/sample3.doc";
    let lit = LiteParse::new(LiteParseConfig::default());
    let data = tokio::fs::read(fixture_path)
        .await
        .expect("Should be able to read file");
    let input = PdfInput::Bytes(data);
    let parsed = lit
        .parse_input(input)
        .await
        .expect("Should be able to parse");
    assert_eq!(parsed.pages.len(), 2);
}

#[tokio::test]
#[serial]
async fn test_parse_image_integration() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let lit = LiteParse::new(LiteParseConfig::default());
    let parsed = lit
        .parse("../../integration_tests_data/receipt.png")
        .await
        .expect("Should be able to parse");
    assert_eq!(parsed.pages.len(), 1);
}

#[tokio::test]
#[serial]
async fn test_parse_office_doc_integration() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let lit = LiteParse::new(LiteParseConfig::default());
    let parsed = lit
        .parse("../../integration_tests_data/sample3.doc")
        .await
        .expect("Should be able to parse");
    assert_eq!(parsed.pages.len(), 2);
}

#[tokio::test]
#[serial]
async fn test_parse_pdf_integration() {
    let lit = LiteParse::new(LiteParseConfig::default());
    let parsed = lit
        .parse("../../integration_tests_data/sample.pdf")
        .await
        .expect("Should be able to parse");
    assert_eq!(parsed.pages.len(), 1);
}

#[tokio::test]
#[serial]
async fn test_parse_bytes_pdf_integration() {
    let fixture_path = "../../integration_tests_data/sample.pdf";
    let lit = LiteParse::new(LiteParseConfig::default());
    let data = tokio::fs::read(fixture_path)
        .await
        .expect("Should be able to read file");
    let input = PdfInput::Bytes(data);
    let parsed = lit
        .parse_input(input)
        .await
        .expect("Should be able to parse");
    assert_eq!(parsed.pages.len(), 1);
}

/// Stress test: many concurrent `parse_input` calls on a multi-threaded
/// tokio runtime through a single `Arc<LiteParse>`. Before the PDFium
/// process-global lock was introduced, this scenario caused malloc
/// double-free / heap corruption because PDFium FFI is not thread-safe.
///
/// We intentionally do **not** use `#[serial]` here — this test must run
/// concurrently with itself (across tasks within the test) to exercise the
/// lock. Other tests in this file are `#[serial]` so they won't race.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_parse_does_not_crash() {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }

    let lit = Arc::new(LiteParse::new(LiteParseConfig {
        ocr_enabled: false,
        quiet: true,
        ..LiteParseConfig::default()
    }));

    let bytes = tokio::fs::read("../../integration_tests_data/sample.pdf")
        .await
        .expect("fixture exists");

    let mut set: JoinSet<usize> = JoinSet::new();
    for _ in 0..16 {
        let lit = lit.clone();
        let bytes = bytes.clone();
        set.spawn(async move {
            let parsed = lit
                .parse_input(PdfInput::Bytes(bytes))
                .await
                .expect("parse should succeed");
            parsed.pages.len()
        });
    }

    let mut total = 0;
    while let Some(joined) = set.join_next().await {
        total += joined.expect("task panicked");
    }
    // 16 tasks × 1 page each
    assert_eq!(total, 16);
}

// -- Vector-glyph (flattened) text detection, issue #291 case 2 --
//
// Both fixtures are dense native-text pages where some body lines are drawn
// as filled vector outlines instead of real text. They cover the two
// flattening profiles seen in the wild:
// - `vector_glyph_text_bezier.pdf`: one Bézier-outline path per glyph,
// - `vector_glyph_text_flattened.pdf`: a whole line as a single path made of
//   ~1800 straight line segments (curves pre-flattened, zero Béziers).

/// The detector must find glyph-like paths in both fixtures and stay quiet on
/// a normal native-text PDF. Runs without OCR, so it's fast.
#[test]
#[serial]
fn test_glyph_like_path_bounds_detects_both_flattening_profiles() {
    let lib = pdfium::Library::init();

    for fixture in [
        "../../integration_tests_data/vector_glyph_text_bezier.pdf",
        "../../integration_tests_data/vector_glyph_text_flattened.pdf",
    ] {
        let doc = lib.load_document(fixture, None).expect("fixture loads");
        let page = doc.page(0).expect("page 0 exists");
        let bounds = page.glyph_like_path_bounds();
        let area: f32 = bounds.iter().map(|b| b.width * b.height).sum();
        assert!(
            !bounds.is_empty(),
            "{fixture}: expected glyph-like paths, found none"
        );
        assert!(
            area > 1000.0,
            "{fixture}: glyph-like path area too small: {area} pt²"
        );
    }

    // Negative control: a normal native-text PDF has no flattened text, so
    // detected area must stay below the OCR gate's threshold (0.3% of page).
    let doc = lib
        .load_document("../../integration_tests_data/sample.pdf", None)
        .expect("sample loads");
    let page = doc.page(0).expect("page 0 exists");
    let bounds = page.glyph_like_path_bounds();
    let area: f32 = bounds.iter().map(|b| b.width * b.height).sum();
    let page_area = page.width() * page.height();
    assert!(
        area / page_area < 0.003,
        "sample.pdf: unexpected glyph-like path area {area} pt² ({}% of page)",
        area / page_area * 100.0
    );
}

/// End-to-end: a dense page with Bézier-outline flattened lines must trigger
/// OCR and recover the flattened text (previously silently dropped).
#[tokio::test]
#[serial]
async fn test_parse_recovers_bezier_outlined_text() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let lit = LiteParse::new(LiteParseConfig {
        quiet: true,
        ..LiteParseConfig::default()
    });
    let parsed = lit
        .parse("../../integration_tests_data/vector_glyph_text_bezier.pdf")
        .await
        .expect("Should be able to parse");
    let text: String = parsed.pages[0]
        .text_items
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    assert!(
        text.contains("approved subcontractors"),
        "flattened line 1 not recovered"
    );
    assert!(
        text.contains("confirm that vendor data"),
        "flattened line 2 not recovered"
    );
}

/// End-to-end: same, for the line-segment-flattened profile (zero Béziers).
#[tokio::test]
#[serial]
async fn test_parse_recovers_line_flattened_text() {
    let env_var = std::env::var("SKIP_INTEGRATION_TESTS");
    if let Ok(v) = env_var
        && v == "yes"
    {
        return;
    }
    let lit = LiteParse::new(LiteParseConfig {
        quiet: true,
        ..LiteParseConfig::default()
    });
    let parsed = lit
        .parse("../../integration_tests_data/vector_glyph_text_flattened.pdf")
        .await
        .expect("Should be able to parse");
    let text: String = parsed.pages[0]
        .text_items
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    assert!(
        text.contains("service-level guarantee"),
        "flattened line not recovered"
    );
}
