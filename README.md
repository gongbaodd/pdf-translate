# pi-docs

BabelDOC-based bilingual PDF translation tooling with a local TypeScript bridge backed by [`@earendil-works/pi-ai`](https://github.com/earendil-works/pi/tree/main/packages/ai).

The root project translates English PDFs into English/Simplified-Chinese side-by-side PDFs. The bridge supports:

- Codex OAuth through `openai-codex`
- OpenCode Go API keys through `opencode-go`
- Direct OpenAI-compatible endpoints through BabelDOC

`pi-cast/` is a separate Apple Silicon podcast/video pipeline.

## Repository map

```text
scripts/translation_bridge.ts   Local OpenAI Chat Completions adapter
scripts/translate_pdf.py        BabelDOC runner and backend selection
test/translation_bridge.test.ts Bridge unit tests
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

Direct API mode bypasses the bridge:

```bash
export OPENAI_API_KEY='...'
uv run python scripts/translate_pdf.py \
  /path/to/input.pdf \
  --backend openai-compatible \
  --model gpt-4o-mini \
  --base-url https://api.openai.com/v1
```

Detailed options and operational notes are in [`docs/translation.md`](docs/translation.md).

## Agent guidance

- Preserve the provider/auth/model separation in `translation_bridge.ts`.
- Keep credentials outside the repository; never log API keys or OAuth tokens.
- Keep the bridge loopback-only and preserve its bearer-token boundary.
- Update `docs/translation.md` and this README when changing login or backend behavior.
- Run `npm run test:adapter`, Python syntax checks, and a backend-specific `--dry-run` after changes.
