# Chinese-only PDF export plan

## Objective

Add a translated-only (monolingual Simplified Chinese) output mode to the
existing BabelDOC wrapper while preserving the current English/Chinese
side-by-side output as the default.

Test target:

```text
pdf/learningdomain-drivendesign.pdf
342 pages, 504 x 661.5 points per page
```

The translated-only result must contain the original page geometry with the
body text replaced by Chinese, without the left-hand English page in a
side-by-side spread.

## Current behavior and findings

- [`scripts/translate_pdf.py`](../../scripts/translate_pdf.py) always builds a
  side-by-side PDF:
  - `build_config()` writes `no-mono = true`, so BabelDOC never emits its
    monolingual PDF (`scripts/translate_pdf.py:113`).
  - `run_translation()` hard-codes the final filename
    `<stem>.en-zh.side-by-side.pdf` (`scripts/translate_pdf.py:245`).
  - `find_output()` prefers names containing `dual` or `bilingual`
    (`scripts/translate_pdf.py:218-236`).
  - `validate_output()` requires a 1008 x 662 point page, i.e. two 504-point
    pages side by side (`scripts/translate_pdf.py:163-168`).
- BabelDOC 0.6.4 already supports monolingual output with `--no-dual`; the
  default `--lang-out` is `zh`.
- Local probe with BabelDOC 0.6.4, no network and `--skip-translation`:
  - `--no-dual --watermark-output-mode no_watermark` on a 504 x 661.5 page
    produced `mono-probe.no_watermark.zh.mono.pdf`.
  - The output page remained 504 x 661.5 points; it was not doubled.
- BabelDOC output naming depends on whether part splitting is active:
  - Single-part run: `<stem>.no_watermark.zh.mono.pdf`
  - Split run with `max-pages-per-part`: merged output
    `<stem>.zh.mono.pdf` (from `ResultMerger`).
- `--pages` selects which pages are translated, not which pages are emitted.
  A page-limited test of this 342-page book therefore still produces 342 output
  pages; unselected pages remain in their original language unless BabelDOC's
  `--only-include-translated-page` is also enabled.
- The existing translation cache is keyed separately from output layout, so a
  Chinese-only export can reuse translations already cached for the same
  provider/model alias.

## Proposed interface

Add one option and keep the existing default unchanged:

```text
--output-mode {bilingual,translated-only}   default: bilingual
```

| Mode | BabelDOC config | Final filename |
| --- | --- | --- |
| `bilingual` | `no-mono = true` | `<stem>.en-zh.side-by-side.pdf` |
| `translated-only` | `no-dual = true` | `<stem>.zh.mono.pdf` |

Rules:

- Set `lang-out = "zh"` explicitly in the generated TOML so output naming and
  glossary language selection do not depend on BabelDOC defaults.
- Never write both `no-mono` and `no-dual`; at least one output format must
  remain enabled.
- `--dry-run` must show `output_mode`, the target output path, and the effective
  `no-mono` / `no-dual` choice without starting the bridge or contacting a
  provider.
- Do not add a `both` mode in this change. It can be added later if one
  translation pass needs to emit both formats.
- Do not change the default mode; existing commands and filenames must keep
  working.

## Implementation steps

### 1. Add the CLI option and target-path helper

File: `scripts/translate_pdf.py`

- Add `--output-mode` with choices `bilingual` and `translated-only`, default
  `bilingual`.
- Extract the final filename logic into a helper, for example:

  ```python
  def final_output_path(input_path: Path, output_dir: Path, output_mode: str) -> Path:
      if output_mode == "translated-only":
          return output_dir / f"{input_path.stem}.zh.mono.pdf"
      return output_dir / f"{input_path.stem}.en-zh.side-by-side.pdf"
  ```

- Include `output_mode`, `output_file`, and the effective
  `babeldoc_no_mono` / `babeldoc_no_dual` values in the dry-run JSON.
- Keep the existing refusal to overwrite an existing final path.

### 2. Generate the correct BabelDOC configuration

File: `scripts/translate_pdf.py`, `build_config()`

- Add explicit `lang-out = "zh"`.
- Replace the unconditional `no-mono = true` line with mode-dependent flags:

  ```text
  bilingual       -> no-mono = true
  translated-only -> no-dual = true
  ```

- Pass `output_mode` into `build_config()` rather than adding another implicit
  argument.
- Leave the bridge/model/cache settings unchanged. Translation requests are
  identical for both output modes, so existing cache entries remain valid.

### 3. Make output discovery mode-aware

File: `scripts/translate_pdf.py`, `find_output()`

- Accept the requested output mode.
- Skip intermediate/debug artifacts, especially paths ending in
  `.decompressed.pdf`.
- For `bilingual`, keep the current preference for `dual` / `bilingual` names.
- For `translated-only`, prefer `.mono.pdf` files and:
  - prefer `<stem>.zh.mono.pdf` when both the merged and no-watermark names
    exist;
  - accept `<stem>.no_watermark.zh.mono.pdf` for single-part runs;
  - fail with a clear list of candidates if zero or multiple unrelated mono PDFs
    are found.
- When the selected source path differs from the final target, keep the current
  atomic `rename` behavior and overwrite check.
- Consider removing a redundant sibling no-watermark mono file only after the
  final file has been validated; otherwise leave BabelDOC artifacts untouched.

### 4. Validate page geometry per output mode

File: `scripts/translate_pdf.py`, `validate_output()`

- Read the source PDF's first-page size and pass it into validation, or pass the
  expected output size from `run_translation()`.
- Expected geometry:
  - `translated-only`: source width x source height, within the existing
    12-point tolerance;
  - `bilingual`: approximately 2 x source width x source height.
- Keep expected page count equal to the source page count. For this book that is
  342 pages in both modes.
- Keep the CJK glyph check on explicit `--pages` or sampled pages. Treat a page
  with no CJK as a warning, not a hard failure, because covers and image-only
  pages may legitimately have no extracted text.
- Update the error text so geometry failures name the selected output mode
  instead of always saying "side-by-side".

### 5. Add offline regression tests

New file: `test/test_translate_pdf.py`

Load `scripts/translate_pdf.py` with `importlib` (the `scripts` directory is not
a package) and cover the pure logic without network access:

- `bilingual` configuration contains `no-mono = true` and no `no-dual`.
- `translated-only` configuration contains `no-dual = true` and no `no-mono`.
- Explicit `lang-out = "zh"` is present.
- Final filename helper returns the existing side-by-side name for
  `bilingual`, and `<stem>.zh.mono.pdf` for `translated-only`.
- `find_output()` selects the merged mono file over the no-watermark variant when
  both exist.
- `find_output()` ignores `.decompressed.pdf` artifacts.
- Dry-run output contains the selected mode and no credential values.

Run with:

```bash
uv run python -m unittest discover -s test -p 'test_*.py'
```

### 6. Update documentation

Files: `README.md`, `docs/translation.md`

- State that the default remains side-by-side.
- Document the new option and exact output names.
- Add a translated-only sample command:

  ```bash
  uv run python scripts/translate_pdf.py \
    'pdf/learningdomain-drivendesign.pdf' \
    --model gpt-6-luna \
    --output-mode translated-only \
    --pages 31,81,151 \
    --output-dir output/zh-only-sample
  ```

- Explain that a page-limited run still emits all source pages and that
  unselected pages remain original-language.
- Explain that the full run writes
  `output/pdf/learningdomain-drivendesign.zh.mono.pdf`.

### 7. Run offline checks

```bash
uv run python -m py_compile scripts/translate_pdf.py
npm run test:adapter
uv run python -m unittest discover -s test -p 'test_*.py'
```

Then confirm both dry runs:

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --model gpt-6-luna \
  --output-mode bilingual \
  --dry-run
```

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --model gpt-6-luna \
  --output-mode translated-only \
  --dry-run
```

Expected differences: only the output path and the `no-mono` / `no-dual` flag.
No bridge process, login, or network request may occur.

### 8. Live sample test on the target PDF

Run a small translated-only sample first. Use the model currently available to
the configured provider; `gpt-6-luna` was the last verified model in this
repository.

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --model gpt-6-luna \
  --output-mode translated-only \
  --pages 31,81,151 \
  --output-dir output/zh-only-sample
```

Inspect the result:

```bash
uv run python - <<'PY'
import pymupdf
from pathlib import Path

path = Path("output/zh-only-sample/learningdomain-drivendesign.zh.mono.pdf")
with pymupdf.open(path) as doc:
    print("pages:", doc.page_count)
    print("size:", doc[0].rect)
    for page_no in (31, 81, 151):
        text = doc[page_no - 1].get_text()
        han = sum("\u3400" <= ch <= "\u9fff" for ch in text)
        print(page_no, "han:", han, "sample:", text[:80].replace("\n", " "))
PY
```

Acceptance checks for the sample:

- Exactly one selected `.zh.mono.pdf` is produced and renamed to the final path.
- 342 pages; page size approximately 504 x 661.5 points.
- Pages 31, 81, and 151 contain Chinese glyphs and no duplicated left-hand
  English page.
- Code blocks and DDD glossary terms remain intact.
- Unselected pages are expected to remain English in this page-limited run.

### 9. Full-book export gate

Only after the sample passes, run the full 342-page export:

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --model gpt-6-luna \
  --output-mode translated-only \
  --output-dir output/pdf
```

Expected output:

```text
output/pdf/learningdomain-drivendesign.zh.mono.pdf
```

This is a live, potentially long-running and billable/rate-limited operation.
Warm cache entries may reduce translation time, but full coverage is not
guaranteed. Do not start it as part of an unattended test.

Final inspection:

- 342 pages and 504 x 661.5 point page size.
- CJK text on representative body pages throughout the book.
- No side-by-side spreads.
- Spot-check diagrams, code blocks, tables, and glossary terminology.
- Confirm the bridge process exits and no temporary config or credential files
  remain.

## Acceptance criteria

- `--output-mode translated-only` produces a valid monolingual Chinese PDF at
  `<stem>.zh.mono.pdf`.
- Default invocations still produce
  `<stem>.en-zh.side-by-side.pdf` with unchanged layout.
- Dry runs expose the selected mode and target path without network or auth side
  effects.
- Output discovery handles both BabelDOC naming variants and rejects ambiguous
  results.
- Validation checks the correct page geometry for each mode.
- Offline tests pass; the live sample passes; the full-book export is available
  as an explicit, separately approved run.

## Risks and edge cases

- BabelDOC uses different mono filenames for single-part and split runs; output
  discovery must not assume one name.
- Page-limited runs do not remove untranslated pages. This is existing BabelDOC
  behavior and should be documented rather than "fixed" implicitly.
- The cover and image-only text may contain no extractable Chinese; validation
  must warn, not fail on those pages.
- Switching modes in the same output directory can collide with an existing
  target file. The existing overwrite refusal must remain.
- Mono output preserves original figures, diagrams, and page furniture; it is a
  text translation, not OCR or a rebuilt document.
- Full-book translation depends on provider credentials, entitlement, rate
  limits, and network access.

## Status

Implemented on 2026-10-29:

- `scripts/translate_pdf.py` supports `--output-mode bilingual|translated-only`,
  mode-aware output discovery and geometry validation.
- `test/test_translate_pdf.py` covers configuration, naming, discovery, geometry,
  and the translated-only dry run.
- `README.md` and `docs/translation.md` document the new mode and filenames.
- Offline checks pass:
  - `uv run python -m py_compile scripts/translate_pdf.py`
  - `uv run python -m unittest discover -s test -p 'test_*.py'` (16 tests)
  - `npm run test:adapter` (4 tests)
- Live 3-page sample passed with `gpt-6-luna`:
  - `output/zh-only-sample/learningdomain-drivendesign.zh.mono.pdf`
  - 342 pages, 504 x 661.5 points
  - Chinese text on pages 31, 81, and 151; diagrams preserved; no side-by-side
    spread.
- Bilingual regression sample passed:
  - `output/bilingual-check/learningdomain-drivendesign.en-zh.side-by-side.pdf`
  - 342 pages, 1008 x 661.5 points.
- Full 342-page translated-only export has not been run. It remains the explicit
  execution gate described in step 9.

