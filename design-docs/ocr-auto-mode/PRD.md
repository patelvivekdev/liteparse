# PRD: `OcrTextMode::Auto` — self-healing native/OCR merge

**Status:** ✅ IMPLEMENTED + tested (opt-in; not yet default). Investigation 2026-06-02, build 2026-06-03.
**Author:** (paired with Claude)
**Related:** PR #244 (`ocr_text_mode` = merge | ocr-only), issue #153/PR #163 (rotation), the OCR-merge dedup regression.

## Implementation summary (what shipped)
- `has_map_error` per-`TextItem`, plumbed from `extract.rs` (`SegmentBuilder`) off
  `TextChar::has_unicode_map_error()` — the primary corruption signal.
- `OcrTextMode::Auto` in `config.rs`, threaded through CLI + python + napi + wasm.
- `arbitrate_auto()` in `ocr_merge.rs`: drop corrupt native (`has_map_error ||
  font_is_buggy`), add OCR that fills the holes/gaps, drop OCR that ≥50%-overlaps
  surviving clean native (`covered_fraction`). 8 behavior unit tests.
- `needs_ocr` also triggers on corrupt native (gated to Auto) so a no-image
  corrupt page still gets OCR'd.
- **Verified:** 99 Rust lib tests green. End-to-end matrix (rebuilt cp312 wheel +
  mock OCR server) on a Class-1 fixture (`12_mixed_native_maperror`):

  | mode | result | native | OCR | note |
  |------|--------|:---:|:---:|------|
  | merge | FAIL | 28 | 0 | keeps control-char garbage |
  | **auto** | **PASS** | **8** | **8** | clean fields stay native, corrupt lines healed by OCR |
  | ocr-only | PASS | 0 | 16 | heals but discards all native fidelity |

  Clean docs unaffected (`7_clean_scan` PASS all modes). Class-3 Caesar
  (`10_mixed`) still FAILs under auto — the documented limitation (§4): no
  structural signal, deferred to Option B.

---

## 1. Problem

On a page that gets OCR'd, LiteParse must decide, where an OCR line overlaps a
native-text box, **who wins**. Today there are only two dumb constant answers
([`ocr_merge.rs:167`](../../crates/liteparse/src/ocr_merge.rs)):

- **`merge`** — native *always* wins; OCR over a native box is dropped as a
  duplicate. → When the native text layer is **corrupt but present**, the
  garbage wins and the correct OCR is discarded.
- **`ocr-only`** — OCR *always* wins; the entire native layer on an OCR'd page
  is thrown away. → Loses the high-fidelity native text (exact spacing, real
  glyphs, perfect numbers) even on the 95% of the page that was fine.

**Goal:** a third mode, **`Auto`**, that decides **per region**: keep native
where it is trustworthy, swap to OCR only where native is corrupt, and fill
image/signature gaps with OCR — at the **same cost as `merge`** (no extra OCR
calls; OCR already runs per-page only where needed). Eventually `Auto` becomes
the default. Confidence scores are explicitly **out of scope** (see §9).

---

## 2. Investigation findings (empirical)

All findings below come from a throwaway probe
([`examples/probe_internals.rs`](../../crates/liteparse/examples/probe_internals.rs))
run on real + synthetic PDFs. The probe dumps, per page, the real extracted
`TextItem`s (bbox, `font_name`, `font_is_buggy`, text) and the raw pdfium
per-character signals (`unicode`, `char_code`, `has_unicode_map_error`,
`is_generated`).

### 2.1 LiteParse already computes a corruption signal — but it has a precondition

`TextItem.font_is_buggy` ([`types.rs:43`](../../crates/liteparse/src/types.rs))
is set during extraction when **all** of:

1. the font **is embedded** (`font_is_embedded == true`), **AND**
2. either:
   - **buggy font name** — `is_buggy_font`: name starts with `TT`, contains
     `+TT` (TrueType subset), or Type1 name `ABCDEF_…` (6 chars + `_`); **or**
   - **buggy codepoint** — `is_buggy_codepoint`: `unicode <= 0x1F` or
     `0xE000 < unicode <= 0xF8FF` (control / Private-Use Area).

The **embedded precondition is critical and easy to miss**: base-14 / non-
embedded fonts are *never* flagged, no matter how corrupt their codepoints.

A second, **independent** signal exists and is currently **unused** anywhere in
the codebase: `TextChar::has_unicode_map_error()`
([`text_page.rs:394`](../../crates/pdfium/src/text_page.rs), wraps
`FPDFText_HasUnicodeMapError`). It is **not** gated on embedding — in testing it
fired on control codepoints in a non-embedded font where `font_is_buggy` did
**not**.

### 2.2 Three classes of native-text corruption

| Class | Mechanism | `font_is_buggy` | `has_unicode_map_error` | Caught today? |
|------:|-----------|:---:|:---:|:---:|
| **1** | Broken/missing ToUnicode → PUA / control codepoints | ✅ (if embedded) | ✅ (often) | yes |
| **2** | Subset font with buggy name (`+TT`, Type1 `ABCDEF_`) | ✅ (if embedded) | sometimes | yes |
| **3** | **Valid-but-wrong** mapping (cipher-like: glyph→plausible wrong Unicode) | ❌ | ❌ | **no** |

Class 3 is the hard one: every character is a *valid, well-mapped* letter, so no
structural signal fires — the text is simply *wrong words*. Detecting it needs
either **agreement with OCR** or a language/dictionary heuristic.

### 2.3 What actually fired on each document

| Document | Kind | `font_is_buggy` | `map_error` | Notes |
|----------|------|:---:|:---:|-------|
| `apple-10k-2024.pdf` (public) | clean native, multi-column | 0 / 65 | 0 | subset fonts `AAAGYH+Helvetica…` correctly **not** flagged |
| **reported invoice form** (local only) | **the real bug**: native + image, rot90 | **2 / 9** | **1237 / 1303** | embedded font with **no name, no ToUnicode**; codepoints are raw glyph indices `0x01,0x02,0x03…` → **Class 1** |
| real client doc A (local only) | clean native + image | 0 / 26 | 0 | standard fonts; was a *language* (pt) + image case, **not** garble |
| real client doc B (local only) | image-only scan | 0 / 0 | 0 | no native text at all |
| `5_corrupt_native_flat.pdf` (our synthetic) | **Caesar garble** | **0 / 25** | **0** | `unicode == char_code` for every char → **Class 3** |
| `cond_control.pdf` (generated, non-embedded) | control codepoints | 0 | **22** | map_error fires, font_is_buggy doesn't (not embedded) |

**Two consequences we must own:**

1. **The real reported doc is confirmed Class 1.** 1237/1303 chars (**95%**) carry
   `has_unicode_map_error`; the embedded font has no name and no ToUnicode, so
   extraction yields raw glyph indices (`0x01, 0x02, …`). `font_is_buggy` fired on
   only **2/9 items**, so **`has_unicode_map_error` is the stronger, more complete
   signal and should be the primary detector.** (§7.1 resolved — the real bug is
   Class 1, the easy/deterministic case.)
2. **Our existing synthetic (`5/9/10/11_*`) is Class 3 — the WRONG model for this
   bug.** It trips no structural signal, but the real corruption trips
   `has_unicode_map_error` on 95% of chars. The Caesar synthetic made the problem
   look harder than it is; we need a **Class-1 fixture** (no-ToUnicode / glyph-index
   font) to exercise `Auto` end-to-end (§8).
3. **Corrupt-page geometry is also degenerate.** On the real doc the 9 surviving
   items had junk boxes (130×130 squares, some negative-`y`) because the broken
   font has no glyph metrics. The detector (map-error) is trustworthy; the bbox
   overlap test must tolerate garbage native geometry (§6).

### 2.4 Segmentation reality: native text is NOT one box per line

This was a specific worry, and the probe confirms it hard. Native `TextItem`s
are frequently **sub-line chunks**, **multiple per line** (columns), and
sometimes **split mid-token**:

- Apple 10-K, same `y=362.8`: `California` | `94-2404110` → two columns.
- Apple 10-K, same row: `Common Stock…` | `AAPL` | `The Nasdaq Stock Market LLC`.
- Apple 10-K **mid-token splits**: item `1` + item `.625% Notes due 2026`;
  item `(I` + item `.R.S. Employer Identification No.)`.
- Our synthetic, `y=258.5`: `Olqh Ghvfulswlrq` | `Sulfh` | `Wrwdo` (3 column chunks).

Meanwhile the **OCR side returns one box per line** (the mock mirrors Azure DI's
full-line-height boxes; real Azure/Tesseract do the same). So:

> **A single OCR line box geometrically overlaps *many* native chunks, possibly
> spanning multiple columns.** The arbitration cannot be "OCR-line vs one native
> box" — it must associate an OCR line with the *set* of native chunks under it
> and decide at chunk granularity, or it will wrongly nuke a clean column when a
> neighbouring column is corrupt.

This is the core design complication and the source of most edge cases (§6).

---

## 3. Proposed design

Add `OcrTextMode::Auto`. On each OCR'd page, for each OCR line result:

```
ocr_box = scaled bbox of the OCR line
natives = native items whose box overlaps ocr_box (with tolerance)

if natives is empty:
    ADD the OCR line              # image / signature / stamp gap-fill
else:
    # decide per overlapping native chunk
    for each n in natives:
        if n.is_corrupt():        # font_is_buggy OR has_unicode_map_error
            mark n for removal
    if any native was marked corrupt:
        remove the corrupt natives that this OCR line covers
        ADD the OCR line (it supplies the corrected text for that region)
    else:
        DROP the OCR line          # native is trustworthy (today's merge behavior)
```

- **`is_corrupt()`** = the structural signals from §2.1. **Primary:
  `has_unicode_map_error`** (caught 95% of chars on the real doc); **secondary:
  `font_is_buggy`** (caught only 2/9 items there). Deterministic, language-
  independent. `font_is_buggy` is already on the item; `has_map_error` needs
  plumbing (§3.1).
- **Cost = `merge`.** No new OCR calls; OCR already runs per-page via the
  existing `needs_ocr` gate. The arbitration is cheap geometry + a flag read.
- **Safety bias:** when in doubt, keep native. We only remove native that is
  *positively flagged* corrupt. A clean document behaves exactly like `merge`.

### 3.1 Plug points
- New enum variant in [`config.rs`](../../crates/liteparse/src/config.rs)
  `OcrTextMode { Merge, OcrOnly, Auto }`, threaded through the bindings exactly
  like the existing two (napi/python/wasm/CLI).
- Logic in [`ocr_merge.rs`](../../crates/liteparse/src/ocr_merge.rs):
  generalize `ocr_results_to_text_items` so the `Merge` path can *also* return
  "native indices to remove," or add an `arbitrate()` that the `Auto` arm calls.
- **Plumb the primary signal:** add a per-item `has_map_error` bool to `TextItem`,
  set true when any non-generated char in the segment has
  `has_unicode_map_error()` — small change in
  [`extract.rs`](../../crates/liteparse/src/extract.rs)’s `SegmentBuilder`.
  `font_is_buggy` is already present as the secondary signal.

### 3.2 Optional `needs_ocr` improvement (separate, smaller change)
Today `needs_ocr` ([`ocr_merge.rs:45`](../../crates/liteparse/src/ocr_merge.rs))
triggers on sparse text or images only. A page **full of corrupt native text**
with no image would **never be OCR'd**, so `Auto` could never repair it. Feeding
"page has many corrupt items" into `needs_ocr` closes that gap. Track as a
follow-up, not part of `Auto` v1.

---

## 4. The Class-3 gap — RESOLVED: real bug is Class 1, build Option A

The real reported doc is **Class 1** (§2.3): 95% map-error, glyph-index font. So
the deterministic structural detector **does** fix the actual bug, and the harder
Class-3 path is **not** needed for v1. Decision: **build Option A.**

- **A. Structural-only `Auto` (v1) ✅ chosen.** Detector = `has_unicode_map_error`
  (primary) + `font_is_buggy` (secondary). Fixes Class 1/2 cleanly and covers the
  real doc. Needs a Class-1 fixture for the e2e matrix (§8); unit tests need no PDF.
- **B. Structural + agreement fallback (deferred).** Only revisit if a *Class-3*
  doc shows up in the wild. Add: when an OCR line *disagrees
  strongly* with the clean-looking native under it, prefer OCR. This catches
  Class 3 too but reintroduces a heuristic (string similarity threshold) and the
- **B. Structural + agreement fallback.** Add: when an OCR line *disagrees
  strongly* with the clean-looking native under it, prefer OCR. This catches
  Class 3 too but reintroduces a heuristic (string similarity threshold) and the
  risk of nuking valid non-dictionary native (IDs, codes, names). Bigger, riskier.

Recommendation: **build A first** (small, safe, deterministic), prove it end-to-
end, then evaluate B as a follow-up *iff* the real doc turns out to be Class 3.

---

## 5. TDD plan (vertical slices)

One test → one bit of implementation → repeat. Tests target **behavior through
the arbitration interface**, mirroring the existing `ocr_merge::tests` style
(construct `TextItem`s + `OcrResult`s directly — **no triggering PDFs needed for
unit tests**, which sidesteps §8 entirely for the core logic).

Tracer bullet, then incremental:

| # | Behavior (test name) | Setup | Expect |
|--:|----------------------|-------|--------|
| 1 | `auto_keeps_clean_native_over_overlapping_ocr` | clean native item + overlapping OCR | native kept, OCR dropped (== merge) |
| 2 | `auto_replaces_buggy_native_with_overlapping_ocr` | native `font_is_buggy=true` + overlapping OCR | buggy native removed, OCR kept |
| 3 | `auto_adds_non_overlapping_ocr` | native somewhere else + OCR over a gap | OCR added (gap fill) |
| 4 | `auto_decides_per_chunk_on_a_mixed_line` | one clean chunk + one buggy chunk at same `y`, one OCR line over both | clean chunk kept, buggy chunk replaced, OCR text present |
| 5 | `auto_does_not_nuke_clean_column_when_other_column_buggy` | two columns, only left buggy, full-width OCR line | right column native preserved |
| 6 | `auto_keeps_buggy_native_when_ocr_empty_or_filtered` | buggy native + OCR conf ≤ 0.1 (filtered) | native kept (don't delete into nothing) |
| 7 | `auto_uses_map_error_signal` | native with `has_map_error=true`, name-clean | treated as corrupt → OCR wins |
| 8 | `auto_leaves_clean_native_untouched_when_no_ocr` | clean native, no OCR results | unchanged |
| 9 | `auto_ignores_low_confidence_ocr_for_gap_fill` | OCR conf ≤ 0.1 over a gap | not added (existing rule preserved) |

Integration (end-to-end, via the ocr-demo matrix, **after** units are green):
- Extend [`run_ocr_only_matrix.py`](../../../liteparse-ocr-demo/run_ocr_only_matrix.py)
  to run a **third** mode `auto` and score recall / garble-leak / native-fidelity
  / CER vs ground truth across modes.
- **Caveat:** the current synthetic is Class 3, so `auto` (structural-only) will
  behave like `merge` on it. A faithful Class-1/2 fixture (§8) is needed to show
  `auto` healing end-to-end. Until then, units (above) are the proof.

---

## 6. Edge cases to cover

- **Multi-column rows** — OCR line spans columns; must not delete a clean column
  (test 5). Reading-order/anchor effects in `projection.rs` are downstream of
  merge and must still produce correct order after native removal/insertion.
- **Mid-token native splits** (`1` + `.625%`) — corruption flag is per-item, so a
  split clean token stays clean; a split buggy token: ensure we don't half-replace.
- **Rotated pages** (rot90/270) — geometry mapping already handled for OCR boxes;
  verify chunk association still works (we have synthetic `11_*`).
- **Whole-page corruption** — no clean anchor; `Auto` should approach `ocr-only`
  for that page without special-casing.
- **Native dropped at extraction** — severely corrupt chars (e.g. control) may be
  filtered out before merge (observed: `cond_control` collapsed to 1 item). Then
  there's no native to block OCR → OCR gap-fills naturally. Verify no double-add.
- **Buggy flag false positives** — confirm clean subset fonts (`AAAGYH+…`,
  observed on Apple 10-K) are **not** flagged (already true; lock with a test).
- **OCR worse than native** — structural-only `Auto` only replaces *flagged*
  native, so a clean-but-OCR-misread region keeps native. Good. (This is why we
  avoid confidence/agreement in v1.)
- **Empty OCR results / all-failed page** — must interoperate with upstream's new
  systemic-failure guard (merged from PR #257) without double-counting.

---

## 7. Open questions

### 7.1 Which corruption class is the real bug? ✅ RESOLVED — Class 1
Probed the real reported doc: 95% of chars carry `has_unicode_map_error`, font has
no ToUnicode (glyph-index codepoints). Structural detection applies; build A.

### 7.2 Plumb `has_unicode_map_error` to the item level? ✅ YES — it is primary
It caught 95% of chars vs `font_is_buggy`'s 2/9 items on the real doc, and fires
without the embedding precondition. Small `extract.rs` change; first slice of work.

### 7.3 Should `Auto` be the default in this PR, or opt-in first?
Recommend opt-in for one release, flip to default once the matrix proves no
regression vs `merge` on clean corpora.

---

## 8. Test fixtures: generating triggering PDFs (findings)

Generating a PDF that trips `font_is_buggy` is **non-trivial** and was *not*
fully achieved in investigation:

- Must **embed** a font (base-14 won't flag). `fitz` embeds via `fontfile=`.
- **Codepoint path:** inserting PUA/control text through `fitz` did not survive
  extraction as PUA (fitz rewrites the ToUnicode; the corrupt line was dropped).
  Needs lower-level control (a hand-built broken ToUnicode CMap via `pikepdf`,
  or a font whose `cmap` maps glyphs into the PUA).
- **Name path (most reliable):** rename an embedded TTF so its name starts with
  `TT` (→ `is_buggy_font`) using `fonttools`, then embed it.
- **Tooling gap:** the ocr-demo venv has `fitz` but **no `pip`, `pikepdf`, or
  `fonttools`**. Add one of these to build the fixtures.
- **Class 3 fixture already exists:** the Caesar synthetics (`5/9/10/11_*`).

Crucially, **unit tests (§5) do not need any of this** — they construct
`TextItem`s with `font_is_buggy`/`has_map_error` set directly. Fixtures are only
for the end-to-end matrix.

---

## 9. Out of scope
- **Confidence scores** — deliberately excluded per direction. OCR confidence is
  high on crisp renders of *both* good and corrupt native, so it cannot *locate*
  corruption; the structural signals do that job.
- **Class-3 detection (option B)** — deferred; revisit only if §7.1 says the real
  bug is Class 3.
- **`needs_ocr` improvement (§3.2)** — separate follow-up.

---

## 10. Rollout
1. Plumb `has_map_error` to `TextItem` (+ unit test at extract level).
2. Add `OcrTextMode::Auto` + arbitration; TDD slices §5 (units).
3. Thread `Auto` through CLI + napi/python/wasm bindings.
4. Build a Class-1/2 fixture (§8); extend the matrix to 3 modes.
5. Opt-in release → measure → flip default.
