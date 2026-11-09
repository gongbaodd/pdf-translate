# GPUI desktop app plan

## Objective

Wrap the existing translation pipeline in a native desktop app built with
[GPUI](https://www.gpui.rs/) (the Rust UI framework behind Zed). The user should be
able to drop a PDF into a window, pick a backend and model, press Translate,
watch progress, and open the result, without using a terminal.

Non-goals for the first release:

- Rewriting the translation logic in Rust. BabelDOC (Python) and the pi-ai
  bridge (TypeScript) stay as they are and run as child processes.
- Replacing the CLI. Every GUI action must still be possible with
  `scripts/translate_pdf.py`, and the CLI stays the source of truth.
- A built-in PDF editor or side-by-side review tool.

## Current architecture

```text
translate_pdf.py  ──spawns──▶  translation_bridge.ts (loopback, bearer token)
       │                                 │
       └──spawns──▶ babeldoc CLI ──HTTP──┘──▶ provider (Codex / OpenCode Go / claude -p)
```

Facts that shape the design:

- `run_translation()` in `scripts/translate_pdf.py` already does all preflight
  checks, builds the temporary BabelDOC config, owns the bridge lifecycle and
  validates the output. The GUI should call this, not reimplement it.
- `--dry-run` prints the full resolved configuration as JSON. This is a
  ready-made "validate settings" call for the GUI.
- Progress is only visible as BabelDOC's human-readable terminal output.
  There is no machine-readable progress or result channel yet.
- Errors are reported as `translate_pdf: <message>` on stderr with exit
  status 2. Again, human-readable only.
- `npm run login` is an interactive readline flow (select prompts, secret
  prompts, "open this URL" notices). A GUI cannot drive it as it is.
- `claude-code` has no bridge login; it depends on the user having run
  `claude` → `/login` once.
- Output refuses to overwrite an existing file. The GUI must show this as a
  clear choice (rename / pick another folder), not a crash.

## Proposed architecture

```text
┌──────────────── pi-docs.app (Rust, GPUI) ────────────────┐
│  UI views  ◀──  AppState entity  ◀──  JobRunner          │
│                                        │ spawns, reads   │
│                                        ▼ NDJSON events   │
│   uv run python scripts/translate_pdf.py --events json …  │
│   node scripts/translation_bridge.ts login --events json  │
└───────────────────────────────────────────────────────────┘
```

The Rust app is a thin shell. It talks to the existing scripts over a small,
versioned, line-delimited JSON protocol on stdout (and stdin for login
answers). Secrets never pass through the Rust process except the OpenCode Go
API key the user types, which is written straight to the login child's stdin.

### Why subprocesses instead of embedding Python

- Keeps the Python and Node environments exactly as pinned in `uv.lock` and
  `package-lock.json`.
- A crash or hang in BabelDOC cannot take the UI down; cancel is a process
  group kill, which `translate_pdf.py` already handles for the bridge
  (`start_new_session=True`, `terminate()`).
- No PyO3/embedded-interpreter packaging work.

## Step 1: Machine-readable events in the scripts

This is the only change to existing code, and it is useful on its own (CI,
other wrappers).

### 1a. `translate_pdf.py --events json`

Add `--events {text,json}` (default `text`, so current behavior is
unchanged). In `json` mode, write one JSON object per line to stdout and send
everything else (BabelDOC output, warnings) to stderr.

```jsonc
{"v":1,"type":"config","config":{...same dict as --dry-run...}}
{"v":1,"type":"stage","stage":"bridge_starting"}
{"v":1,"type":"stage","stage":"bridge_ready"}
{"v":1,"type":"stage","stage":"translating"}
{"v":1,"type":"progress","stage":"<babeldoc stage name>","current":12,"total":342,"overall":0.18}
{"v":1,"type":"warning","message":"no Chinese glyph extracted from output page 7"}
{"v":1,"type":"result","output_file":"/…/book.zh.mono.pdf","pages":342}
{"v":1,"type":"error","code":"output_exists","message":"refusing to overwrite existing output: …"}
```

Rules:

- Add a `code` field to `TranslationError` (`input_missing`,
  `output_exists`, `missing_api_key`, `auth_missing`, `bridge_failed`,
  `babeldoc_failed`, `validation_failed`, …) so the GUI can map errors to
  actions instead of parsing strings.
- `--dry-run --events json` emits a single `config` event and exits 0.
- Never put API keys, bearer tokens or OAuth tokens into any event. Reuse the
  redaction already used for the dry-run config (`<per-run-local-bearer-token>`).

### 1b. Progress source

BabelDOC 0.6.4 reports progress through its Python API
(`babeldoc.format.pdf.high_level.async_translate` yields `progress_update`,
`finish` and `error` events) but the wrapper currently runs the `babeldoc` CLI.
Two options:

| Option | Work | Risk |
| --- | --- | --- |
| A. Keep the CLI, parse its progress lines from stderr | Small | Breaks if BabelDOC changes its output format |
| B. Call `async_translate` in-process from `translate_pdf.py` | Medium | Must rebuild the config from Python objects instead of TOML |

Recommendation: ship A first behind the `--events json` flag with a
"progress unknown" fallback (indeterminate bar), and move to B in a follow-up
once the GUI is working. Pin the BabelDOC version check so a version bump fails
loudly in tests rather than silently in the GUI.

### 1c. `translation_bridge.ts login --events json`

Turn the existing `prompt` / `notify` callbacks in `runLogin()` into NDJSON:

```jsonc
// bridge → GUI (stdout)
{"v":1,"type":"select","id":"p1","message":"…","options":[{"id":"…","label":"…"}]}
{"v":1,"type":"secret","id":"p2","message":"OpenCode Go API key"}
{"v":1,"type":"text","id":"p3","message":"…","placeholder":"…"}
{"v":1,"type":"auth_url","url":"https://…","instructions":"…"}
{"v":1,"type":"device_code","verification_uri":"https://…","user_code":"ABCD-1234"}
{"v":1,"type":"done","auth_file":"~/.config/…/auth.json"}
// GUI → bridge (stdin)
{"id":"p2","value":"sk-…"}
```

The credential store, file mode `600`, locking and atomic writes stay exactly
as they are. The GUI only renders prompts and forwards answers.

Also add `status --provider <name> [--auth-file …] --events json` that reports
whether a usable credential exists, without printing it. The GUI uses this to
show "Signed in" / "Sign in" per backend.

### 1d. Tests

- Python: event stream for dry-run, for each error code, and a fake-BabelDOC
  run (existing test patterns in `test/test_translate_pdf.py`).
- Node: login event flow with a fake provider, and that no event contains the
  secret value (extend `test/translation_bridge.test.ts`).

## Step 2: Rust crate skeleton

Add a new directory, separate from the scripts:

```text
desktop/
  Cargo.toml
  src/
    main.rs          App::new().run(...), window, menus, key bindings
    state.rs         AppState entity: settings, jobs, login status
    settings.rs      persisted settings (backend, model, output dir, glossary…)
    runner.rs        spawns scripts, reads NDJSON, maps to events
    protocol.rs      serde types for the v1 event schema
    views/
      root.rs        main window layout
      drop_zone.rs   PDF drop target + file picker
      options.rs     backend/model/mode/pages/glossary form
      job_list.rs    queued/running/finished jobs with progress
      login.rs       sign-in sheet driven by login events
      errors.rs      error banners mapped from error codes
```

Dependencies:

- `gpui` (pin an exact version or git rev; GPUI's API changes often)
- (originally `gpui-component`; replaced by small in-crate widgets, see Implementation status)
- `serde`, `serde_json` for the protocol
- `smol` or GPUI's executors for async process I/O
- `directories` for config paths

Locating the repo: in development the app finds the repo root by walking up
from the executable or reading `PI_DOCS_ROOT`. Packaging (Step 6) replaces
this with a bundled runtime.

## Step 3: Core UI (first usable build)

Single window, three areas:

1. **Input** — drop zone (`on_drop::<ExternalPaths>`) plus "Choose PDF…"
   (`cx.prompt_for_paths`). Shows file name, page count and page size (from a
   `--dry-run` call, or a tiny helper that reuses `page_count_from_pdf`).
2. **Options** — mirrors the CLI flags:
   - Backend: Codex / OpenCode Go / Claude Code / OpenAI-compatible, with a
     sign-in status badge per backend.
   - Model: free text with suggestions (OpenCode Go model list from pi-ai,
     Claude aliases `haiku` / `sonnet` / `opus`).
   - Output mode: Bilingual side-by-side / Chinese only.
   - Pages (optional), concurrency (1–16, shown only where useful), glossary
     picker (defaults to `glossary/ddd-en-zh.csv`), code-font regex under
     "Advanced".
   - Output folder picker.
   - For OpenAI-compatible: base URL and the *name* of the API-key env var.
     The app does not store raw API keys for this mode in the first release.
3. **Jobs** — one row per run: status, progress bar, elapsed time, "Open",
   "Show in Finder", "Cancel", "Show log".

Behavior:

- Every option change triggers a debounced `--dry-run --events json`; its
  errors show inline before the user presses Translate.
- One job runs at a time in the first release; extra jobs queue. Parallel
  jobs would multiply provider load and bridge processes.
- Cancel sends SIGTERM to the process group, then SIGKILL after a grace
  period. `translate_pdf.py`'s `finally` already stops the bridge and deletes
  the temporary config.
- `output_exists` errors offer "Choose another folder" or "Reveal existing
  file". The app never deletes outputs on its own.
- Raw stderr is kept per job (in memory, capped) for "Show log", with no
  secrets since the scripts never print them.

GPUI patterns to use:

- `AppState` as an `Entity`; views subscribe and call `cx.notify()` on change.
- `cx.spawn(async move |this, cx| …)` to read child stdout line by line and
  `this.update(cx, …)` per event. CPU-light, so no background executor needed
  beyond process I/O.
- Actions + key bindings: `cmd-o` open, `cmd-enter` translate, `cmd-.`
  cancel, `cmd-,` settings.

## Step 4: Sign-in

- Sign-in sheet per backend, driven entirely by the Step 1c events:
  `select` → list, `secret` → masked input, `auth_url` → "Open in browser"
  button (`cx.open_url`) plus copyable URL, `device_code` → large code with copy
  button.
- `claude-code`: no bridge login. Show whether `claude` is on `PATH`
  (`claude --version`) and tell the user to run `claude` → `/login` once. Allow
  overriding the binary path (maps to `--claude-bin`).
- Sign-in status refreshes on launch and after each login.

## Step 5: Settings and polish

- Persist non-secret settings to
  `~/Library/Application Support/pi-docs/settings.json` (platform equivalent
  elsewhere). Never persist secrets there.
- Remember last backend, model, output mode, output folder and glossary.
- Light/dark theme following the system.
- macOS notification when a long job finishes.
- Optional preview: render the first translated page as a thumbnail with
  `pdfium-render` or MuPDF and show it via GPUI `img()`. The default action
  stays "Open" in the system PDF viewer.

## Step 6: Packaging

Target macOS (Apple Silicon) first; GPUI also runs on Linux and Windows, but
BabelDOC asset setup and Claude Code paths are only tested on macOS today.

Two stages:

1. **Developer build** — `cargo run -p pi-docs-desktop` inside the repo.
   Requires `uv`, Node ≥ 22.19 and `npm ci` / `uv sync` already done. The app
   checks these on launch and shows exactly which command is missing.
2. **Standalone `.app`** —
   - Bundle the `scripts/`, `glossary/`, `pyproject.toml`, `uv.lock`,
     `package.json`, `package-lock.json` as app resources.
   - Bundle a pinned `uv` binary and a pinned Node runtime.
   - On first launch, create the Python venv and `node_modules` under
     Application Support (`uv sync --frozen`, `npm ci`) and download BabelDOC
     assets, with a progress screen. Doing this at first launch keeps the
     download small and avoids signing hundreds of Python binaries in the bundle.
   - Build with `cargo bundle` (or a small script around `codesign` /
     `notarytool`); sign and notarize for distribution.

## Step 7: Verification

- Unit tests for `protocol.rs` against recorded NDJSON fixtures produced by
  the Step 1 tests, so the Rust and Python sides cannot drift apart silently.
- Integration test: run the app's runner against `translate_pdf.py --dry-run`
  for every backend.
- Manual checklist per release:
  - Drop a PDF, dry-run errors show for a missing credential.
  - OpenCode Go sign-in with a test key; auth file is mode `600`.
  - Codex OAuth sign-in opens the browser and completes.
  - 3-page translation (`--pages 31,81,151`) in both output modes for at
    least one bridge backend; output opens.
  - Cancel mid-run leaves no `node` / `babeldoc` / `claude` processes.
  - Existing output triggers the overwrite choice, not a crash.
- Keep the README "Agent guidance" checks: `npm run test:adapter`, Python
  syntax checks, backend `--dry-run`.

## Milestones

| # | Deliverable | Depends on |
| --- | --- | --- |
| M1 | `--events json` + error codes in `translate_pdf.py`, tests | — |
| M2 | Bridge `login --events json` and `status`, tests | — |
| M3 | GPUI window: pick file, options, dry-run validation, run, progress, open result | M1 |
| M4 | Sign-in sheet for Codex and OpenCode Go, Claude Code status | M2, M3 |
| M5 | Settings persistence, queue, cancel, notifications | M3 |
| M6 | Signed standalone macOS `.app` with first-run setup | M5 |
| M7 | (Optional) In-process BabelDOC progress, page thumbnail preview | M3 |

## Decisions

- One PDF at a time. Extra jobs queue and run sequentially.
- OpenAI-compatible API keys stay environment-variable only (no Keychain).
- Windows and Linux are supported targets (macOS, Windows and Linux paths are
  handled in the runner and bridge; only macOS has been run so far).
- `pi-cast/` stays a separate project.

## Implementation status

Implemented in `desktop/` and the scripts:

- M1: `translate_pdf.py --events json`, error codes, SIGTERM handling,
  best-effort progress parsing (option A). Tests in `test/test_translate_pdf.py`.
- M2: `translation_bridge.ts login --events json` and `status`. Tests in
  `test/translation_bridge.test.ts`. The credential store no longer fails the
  permission check on Windows and uses `os.homedir()` for the default path.
- M3: GPUI window with drop zone / file picker, backend, model, output mode,
  output folder, advanced options, live dry-run validation, run, progress,
  cancel, open result, log view.
- M4: in-app sign-in sheet (select / secret / text / auth URL / device code) for
  Codex and OpenCode Go; Claude Code CLI detection.
- M5 (partly): settings persistence, job queue, cancel with a 10 s grace
  period. Not done: desktop notifications, dark theme.

Differences from the plan above:

- Plain `gpui` 0.2.2 is used instead of `gpui-component`; `text_input.rs` is
  adapted from GPUI's own example. The `runtime_shaders` feature is enabled so
  building on macOS does not need Apple's Metal Toolchain.
- The input card shows the file name only. Page count and size are not shown
  (the dry-run does not report them).
- Not done yet: M6 (signed standalone `.app`/installer with first-run setup)
  and M7 (in-process BabelDOC progress, page thumbnail).
- The app was compiled and started on macOS, but its window has not been
  visually reviewed, and Windows/Linux builds have not been tried.

## Documentation updates

When this lands, update:

- `README.md` repository map (`desktop/`) and a short "Desktop app" section.
- `docs/translation.md` with the `--events json` protocol and the bridge
  `login --events json` / `status` commands.
