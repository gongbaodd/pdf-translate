# BabelDOC translation commands

The repository contains a pinned Python environment for BabelDOC and a pinned
Node dependency for the pi-ai subscription bridges.

## Install

```bash
uv sync
npm ci
```

Run the offline adapter checks:

```bash
npm run test:adapter
```

BabelDOC assets and layout models are downloaded once:

```bash
uv run babeldoc --warmup
```

The checked-in input is `learningdomain-drivendesign.pdf`. The wrapper defaults
to that file and writes the final bilingual PDF to
`output/pdf/learningdomain-drivendesign.en-zh.side-by-side.pdf`.

Pass another PDF as the positional input argument. The output filename uses
that PDF's stem:

```bash
uv run python scripts/translate_pdf.py \
  /path/to/another-book.pdf \
  --model gpt-6-luna \
  --output-dir output/another-book
```

This writes:

```text
output/another-book/another-book.en-zh.side-by-side.pdf
```

## Provider backends

The bridge supports ChatGPT/Codex OAuth through pi-ai's `openai-codex` provider
and OpenCode Go API keys through pi-ai's `opencode-go` provider. Neither route
uses `OPENAI_API_KEY` or invokes the Codex coding-agent CLI. Credentials are
stored outside the repository in provider-specific files:

- Codex: `~/.config/babeldoc-codex/auth.json`
- OpenCode Go: `~/.config/babeldoc-opencode-go/auth.json`

### Codex OAuth

Log in once, interactively. The command creates the parent directory and stores
the credential atomically with mode `600`:

```bash
npm run login -- --provider codex
```

If an earlier login stopped after the browser callback, rerun the same command.
The OAuth flow is safe to repeat and does not require manually creating the
credential directory.

### OpenCode Go API key

OpenCode Go uses an API key, not OAuth. Subscribe and create the key at
[opencode.ai/auth](https://opencode.ai/zen), then paste it into the interactive
login:

```bash
npm run login:opencode-go
```

The command stores the key at
`~/.config/babeldoc-opencode-go/auth.json` with mode `600`. Override the
location with `--auth-file`. The OpenCode Go bridge sends a stable
`x-opencode-session` header for each bridge run, as required for routing and
prompt caching.

List pinned models from either installed provider catalog:

```bash
node --input-type=module -e 'import { opencodeGoProvider } from "@earendil-works/pi-ai/providers/opencode-go"; console.log(opencodeGoProvider().getModels().map(model => model.id).join("\n"))'
```

Run a no-network configuration check first. `--dry-run` does not start the
bridge, refresh credentials, or call BabelDOC:

```bash
uv run python scripts/translate_pdf.py \
  --model gpt-5.6-luna \
  --dry-run
```

Use OpenCode Go for the sample by selecting its backend explicitly:

```bash
uv run python scripts/translate_pdf.py \
  --backend opencode-go \
  --model gpt-5.6-luna \
  --pages 31,81,151 \
  --output-dir output/opencode-go-sample
```

Translate the three-page sample into a separate directory so the validated
sample does not block the later full-book output:

```bash
uv run python scripts/translate_pdf.py \
  --model gpt-5.6-luna \
  --pages 31,81,151 \
  --output-dir output/sample
```

The wrapper starts a loopback-only bridge with a random per-run bearer token,
passes only that token to BabelDOC, limits the bridge to one in-flight request,
and terminates the bridge on completion, failure, or interruption.

`temperature=0` is accepted for BabelDOC compatibility but omitted from the
provider request because OpenCode Go and the Codex endpoint reject it.

For page-limited runs, the wrapper disables BabelDOC part splitting. BabelDOC
0.6.4 can otherwise create empty split parts when all selected pages fall
outside the first 25-page part.

If BabelDOC fails before producing a PDF, repeat the command with `--debug` to
preserve its traceback and working artifacts:

```bash
uv run python scripts/translate_pdf.py \
  --model gpt-6-luna \
  --pages 31,81,151 \
  --output-dir output/sample \
  --debug
```

## Direct OpenAI-compatible backend

This route bypasses the bridge and reads the API key from the environment:

```bash
export OPENAI_API_KEY='...'
uv run python scripts/translate_pdf.py \
  --backend openai-compatible \
  --model gpt-4o-mini \
  --base-url https://api.openai.com/v1 \
  --pages 31,81,151
```

Use `--api-key-env NAME` when the endpoint uses another environment variable.
The wrapper writes the API key only to a mode-600 temporary BabelDOC config and
never includes it in the process argument list or logs.

## Full run

After inspecting the sample PDF, omit `--pages`:

```bash
uv run python scripts/translate_pdf.py --model gpt-5.6-luna
```

The wrapper uses English-to-Simplified-Chinese side-by-side mode, the checked-in
DDD glossary, 25-page parts, no watermark, no monolingual output, and one
worker for the selected bridge backend. BabelDOC's translation cache remains
enabled.

## Security and operational notes

- Do not commit OAuth credentials, API keys, generated PDFs, or BabelDOC working
  data. The repository ignore rules cover the local credential file and output.
- A ChatGPT subscription is not an OpenAI API key. Select the direct backend only
  when API billing and an OpenAI-compatible endpoint are intended.
- The Codex provider is currently labeled legacy upstream. Keep the versions in
  `package-lock.json` and `pyproject.toml` pinned and revalidate the bridge when
  changing either dependency.
- The sample and full run require network access and valid provider credentials;
  no live translation is performed by installation or dry-run commands.
