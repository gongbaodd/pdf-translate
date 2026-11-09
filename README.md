# pi-docs

BabelDOC-based bilingual PDF translation tooling with a local TypeScript bridge backed by [`@earendil-works/pi-ai`](https://github.com/earendil-works/pi/tree/main/packages/ai).

The root project translates English PDFs into English/Simplified-Chinese side-by-side PDFs or Chinese-only monolingual PDFs. The bridge supports:

- Codex OAuth through `openai-codex`
- OpenCode Go API keys through `opencode-go`
- A Claude Pro/Max subscription through the local Claude Code CLI (`claude-code`)
- Direct OpenAI-compatible endpoints through BabelDOC

`desktop/` is a native GPUI desktop app that wraps the same pipeline (see [Desktop app](#desktop-app)).

`pi-cast/` is a separate Apple Silicon podcast/video pipeline.

## Repository map

```text
scripts/translation_bridge.ts   Local OpenAI Chat Completions adapter
scripts/translate_pdf.py        BabelDOC runner and backend selection
test/translation_bridge.test.ts Bridge unit tests
desktop/                        GPUI desktop app (Rust)
docs/translation.md             Installation and operational instructions
docs/plan/                      Design notes and implementation plan
package.json                    Node commands and pinned pi-ai dependency
pyproject.toml                  Pinned Python/BabelDOC environment
```

## Requirements

- Node.js >= 22.19
- `npm`
- Python 3.12 and `uv`
- BabelDOC and its model assets, installed by the pinned Python environment

Install dependencies:

```bash
npm ci
uv sync
```

Run the adapter tests:

```bash
npm run test:adapter
```

## OpenCode Go login

OpenCode Go uses an API key from the OpenCode Console. It does not use OAuth and does not use `OPENAI_API_KEY`.

```bash
npm run login:opencode-go
```

The default credential file is:

```text
~/.config/babeldoc-opencode-go/auth.json
```

Override it when needed:

```bash
npm run login:opencode-go -- \
  --auth-file "$HOME/.config/babeldoc-opencode-go/auth.json"
```

The login command creates the parent directory, writes atomically, and restricts credentials to mode `600`. Do not place credentials in the repository.

List the installed OpenCode Go models:

```bash
node --input-type=module -e \
  'import { opencodeGoProvider } from "@earendil-works/pi-ai/providers/opencode-go"; console.log(opencodeGoProvider().getModels().map(model => model.id).join("\n"))'
```

## Translation

Use OpenCode Go explicitly:

```bash
uv run python scripts/translate_pdf.py \
  /path/to/input.pdf \
  --backend opencode-go \
  --model gpt-5.6-luna \
  --pages 31,81,151 \
  --output-dir output/opencode-go-sample
```

Use Codex OAuth instead:

```bash
npm run login -- --provider codex
uv run python scripts/translate_pdf.py \
  /path/to/input.pdf \
  --backend codex \
  --model gpt-5.6-luna
```

Run a configuration-only check before a networked translation:

```bash
uv run python scripts/translate_pdf.py \
  /path/to/input.pdf \
  --backend opencode-go \
  --model gpt-5.6-luna \
  --dry-run
```

The wrapper starts a loopback-only bridge, uses a random per-run bearer token between BabelDOC and the bridge, limits bridge concurrency to one request, and terminates the bridge after the run. OpenCode Go requests receive a stable `x-opencode-session` header for the bridge run.

Use your Claude subscription through the Claude Code CLI. There is no bridge
login; sign in once with `claude` (`/login`) and the bridge runs `claude -p`
for each BabelDOC request:

```bash
uv run python scripts/translate_pdf.py \
  /path/to/input.pdf \
  --backend claude-code \
  --model sonnet \
  --pages 31,81,151 \
  --output-dir output/claude-code-sample
```

`--model` accepts Claude Code aliases (`haiku`, `sonnet`, `opus`) or full model
IDs. Add `--concurrency 6` to run several `claude` processes in
parallel and `--glossary glossary/testing-genai-en-zh.csv` to swap the default
DDD glossary. See [`docs/translation.md`](docs/translation.md#claude-code-cli) for
details and limits.

Direct API mode bypasses the bridge:

```bash
export OPENAI_API_KEY='...'
uv run python scripts/translate_pdf.py \
  /path/to/input.pdf \
  --backend openai-compatible \
  --model gpt-4o-mini \
  --base-url https://api.openai.com/v1
```

Emit a Chinese-only monolingual PDF instead of the side-by-side PDF:

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --backend opencode-go \
  --model gpt-5.6-luna \
  --output-mode translated-only \
  --output-dir output/zh-only
```

This writes `output/zh-only/learningdomain-drivendesign.zh.mono.pdf` at the
source page size. The default remains `--output-mode bilingual`.

Detailed options and operational notes are in [`docs/translation.md`](docs/translation.md).

## Desktop app

`desktop/` is a [GPUI](https://www.gpui.rs/) app. It runs the scripts above as
child processes and reads their `--events json` output, so the CLI stays the
source of truth.

```bash
npm ci && uv sync          # once, as above
cd desktop
cargo run --release
```

The app finds the repository by walking up from the working directory, or from
`PI_DOCS_ROOT`. Start it from a shell if you use OpenAI-compatible mode, because
the API key is read from the environment variable you name. Design and status:
[`docs/plan/gpui-desktop-app.md`](docs/plan/gpui-desktop-app.md).

```bash
cd desktop && cargo test
```

## Agent guidance

- Preserve the provider/auth/model separation in `translation_bridge.ts`.
- Keep credentials outside the repository; never log API keys or OAuth tokens.
- Keep the bridge loopback-only and preserve its bearer-token boundary.
- Update `docs/translation.md` and this README when changing login or backend behavior.
- Keep the `--events json` protocol (`docs/translation.md`) and `desktop/src/protocol.rs` in sync.
- Run `npm run test:adapter`, Python syntax checks, and a backend-specific `--dry-run` after changes.
