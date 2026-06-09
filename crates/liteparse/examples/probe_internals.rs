//! Throwaway investigation probe for the OCR "Auto" design work.
//!
//! Dumps, for a given PDF:
//!   * ITEM view  — the real extracted TextItems: bbox, font_name, font_is_buggy,
//!                  and (escaped) text. Reveals segmentation (full-line vs sub-line
//!                  chunks) and which native runs LiteParse already flags as buggy.
//!   * CHAR view  — raw pdfium per-character signals: unicode, char_code,
//!                  has_unicode_map_error, is_generated, plus per-font char counts.
//!
//! Usage:
//!   cargo run --example probe_internals -- <pdf> [max_pages=3] [chars]
//!
//! PUA codepoints are rendered as [PUA:XXXX] and control chars as \u{..} so
//! corrupt/garbled native text is visible in a terminal.

use std::collections::BTreeMap;

use liteparse::extract::extract_pages_from_input;
use liteparse::types::PdfInput;
use pdfium::Library;

fn esc(s: &str) -> String {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if (0xE000..=0xF8FF).contains(&u) {
                format!("[PUA:{u:04X}]")
            } else if c.is_control() {
                format!("\\u{{{u:04x}}}")
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: probe_internals <pdf> [max_pages=3] [chars]");
        std::process::exit(2);
    }
    let path = args[1].clone();
    let max_pages: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let dump_chars = args.iter().any(|a| a == "chars");

    // ---- ITEM view (real extraction: segmentation + font_is_buggy) ----
    let pages = extract_pages_from_input(&PdfInput::Path(path.clone()), None, max_pages, None)
        .expect("extract failed");
    for p in &pages {
        let buggy = p.text_items.iter().filter(|t| t.font_is_buggy).count();
        println!(
            "===== PAGE {} ({:.0}x{:.0})  items={}  font_is_buggy={}/{} =====",
            p.page_number,
            p.page_width,
            p.page_height,
            p.text_items.len(),
            buggy,
            p.text_items.len()
        );
        for (i, t) in p.text_items.iter().enumerate() {
            println!(
                "  [{:>3}] x={:>6.1} y={:>6.1} w={:>6.1} h={:>4.1} buggy={} merr={} font={:<22} | {}",
                i,
                t.x,
                t.y,
                t.width,
                t.height,
                if t.font_is_buggy { "Y" } else { "." },
                if t.has_map_error { "Y" } else { "." },
                truncate(&t.font_name.clone().unwrap_or_default(), 22),
                truncate(&esc(&t.text), 70),
            );
        }
        println!();
    }

    if !dump_chars {
        return;
    }

    // ---- CHAR view (raw pdfium signals) ----
    let lib = Library::init();
    let doc = lib.load_document(&path, None).expect("load failed");
    let n = doc.page_count().min(max_pages as i32);
    for pi in 0..n {
        let page = doc.page(pi).expect("page");
        let tp = page.text().expect("text page");
        let cc = tp.char_count();
        let (mut map_err, mut generated, mut pua, mut ctrl) = (0u32, 0u32, 0u32, 0u32);
        let mut fonts: BTreeMap<String, (i32, u32)> = BTreeMap::new();
        for i in 0..cc {
            let Some(ch) = tp.char_at(i) else { continue };
            let u = ch.unicode();
            if ch.has_unicode_map_error() {
                map_err += 1;
            }
            if ch.is_generated() {
                generated += 1;
            }
            if (0xE000..=0xF8FF).contains(&u) {
                pua += 1;
            }
            if u <= 0x1F {
                ctrl += 1;
            }
            if let Some((name, flags)) = ch.font_info() {
                let e = fonts.entry(name).or_insert((flags, 0));
                e.1 += 1;
            }
        }
        println!(
            "===== PAGE {} CHARS  total={}  map_error={}  generated={}  pua={}  control={} =====",
            pi + 1,
            cc,
            map_err,
            generated,
            pua,
            ctrl
        );
        for (name, (flags, count)) in &fonts {
            println!("  font: {name:<30} flags={flags:#06x} chars={count}");
        }
        // sample the first ~24 real (non-generated) chars to see raw signals
        let mut shown = 0;
        for i in 0..cc {
            if shown >= 24 {
                break;
            }
            let Some(ch) = tp.char_at(i) else { continue };
            if ch.is_generated() {
                continue;
            }
            let u = ch.unicode();
            let disp = char::from_u32(u)
                .filter(|c| !c.is_control())
                .unwrap_or('·');
            println!(
                "    char[{:>3}] u={:#06x} '{}'  code={:#06x}  map_err={}",
                i,
                u,
                disp,
                ch.char_code(),
                ch.has_unicode_map_error()
            );
            shown += 1;
        }
        println!();
    }
}
