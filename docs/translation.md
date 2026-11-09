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

## Output modes

The checked-in input is `learningdomain-drivendesign.pdf`. With the default
`--output-mode bilingual`, the wrapper defaults to that file and writes the final
bilingual PDF to
`output/pdf/learningdomain-drivendesign.en-zh.side-by-side.pdf`.

Use `--output-mode translated-only` for a monolingual Simplified Chinese PDF.
The wrapper normalizes BabelDOC's monolingual output to
`<stem>.zh.mono.pdf`, for example:

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --model gpt-6-luna \
  --output-mode translated-only \
  --output-dir output/zh-only
```

This writes:

```text
output/zh-only/learningdomain-drivendesign.zh.mono.pdf
```

The translated-only PDF keeps the source page size and replaces page text with
Chinese; it is not a side-by-side spread.

Page selection with `--pages` controls which pages are translated, not which
pages are emitted. A page-limited run of a 342-page book still produces 342
output pages, and unselected pages remain in their original language.

## Custom input

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

The bridge supports ChatGPT/Codex OAuth through pi-ai's `openai-codex` provider,
OpenCode Go API keys through pi-ai's `opencode-go` provider, and a Claude
subscription through the local Claude Code CLI (`claude-code`, see below).
The pi-ai routes do not use `OPENAI_API_KEY` or invoke the Codex coding-agent
CLI. Their credentials are stored outside the repository in provider-specific
files:

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

### Claude Code CLI

The `claude-code` backend uses your Claude Pro/Max subscription by running the
official Claude Code CLI (`claude -p`) once per BabelDOC request. It does not
use pi-ai or store any credential of its own: Claude Code keeps its own sign-in.
This is the supported way to spend subscription usage from a script; logging a
third-party tool into a Claude subscription via OAuth is not permitted by
Anthropic's terms.

Sign in once, interactively, with your subscription account:

```bash
claude        # then run /login and choose your Claude subscription
```

Translate a sample:

```bash
uv run python scripts/translate_pdf.py \
  --backend claude-code \
  --model sonnet \
  --pages 31,81,151 \
  --output-dir output/claude-code-sample
```

Behavior:

- `--model` takes a Claude Code alias (`haiku`, `sonnet`, `opus`) or a full
  model ID; the BabelDOC alias is `claude-code/<model>`.
- Each request runs `claude -p --output-format json --tools "" --system-prompt
  ... --no-session-persistence --strict-mcp-config --setting-sources ""
  --disable-slash-commands` from an empty temporary directory, so no tools,
  MCP servers, settings, or project `CLAUDE.md` are loaded. BabelDOC's text is
  sent on stdin.
- `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` are removed from the CLI's
  environment so usage goes to the signed-in subscription rather than API
  billing. `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) is kept.
- Use `--claude-bin /path/to/claude` (or `CLAUDE_BIN`) when `claude` is not on
  `PATH`. `--auth-file` is rejected for this backend, and
  `npm run login -- --provider claude-code` only prints sign-in instructions.
- Each request is limited to 10 minutes. Subscription limit errors
  ("usage limit reached") are returned to BabelDOC as HTTP 429, sign-in errors
  as HTTP 401.

Parallelism: `--concurrency N` (1-16, default 1) lets the bridge run N
requests at once and sets BabelDOC's `pool-max-workers`,
`term-pool-max-workers`, and default `qps` to N. With `claude-code`,
`--concurrency 6` translated a 306-page book at roughly 4-5 minutes per
25-page part. Keep the default of 1 for the pi-ai backends unless the
provider tolerates parallel requests.

Glossary: `--glossary PATH` replaces the default DDD glossary
(`glossary/ddd-en-zh.csv`). Use `glossary/testing-genai-en-zh.csv` for
software-testing / LLM books, or add a new `source,target` CSV per book.

Code listings: some books set code in a font BabelDOC does not recognize, and
it then rewraps and translates the listing. Pass `--code-font-pattern 'Courier.*'`
(a regex of font names) to keep those fonts verbatim. This was needed for
`Software_Testing_with_Generative_AI.pdf` (code font `CourierStd`).

Limits: every request starts a new CLI process (about 5 seconds each) and
carries Claude Code's base context, mostly served from the prompt cache.
A full book is thousands of requests, so expect a long run and noticeable
subscription usage. Start with `--pages`, prefer `haiku` or `sonnet` for bulk
work, and rerun after a limit reset: BabelDOC's translation cache skips text
that was already translated.

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

The default `bilingual` mode uses English-to-Simplified-Chinese side-by-side
layout, the checked-in DDD glossary, 25-page parts, no watermark, no monolingual
output, and one worker for the selected bridge backend. BabelDOC's translation
cache remains enabled.

For a Chinese-only full run:

```bash
uv run python scripts/translate_pdf.py \
  'pdf/learningdomain-drivendesign.pdf' \
  --model gpt-5.6-luna \
  --output-mode translated-only \
  --output-dir output/pdf
```

This writes `output/pdf/learningdomain-drivendesign.zh.mono.pdf`. The same
glossary, caching, part splitting, and concurrency limits apply.

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

## Machine-readable events (`--events json`)

`translate_pdf.py --events json` writes one JSON object per line to stdout
(version `v: 1`); BabelDOC output and human messages go to stderr. The desktop
app uses this. Default `--events text` output is unchanged.

| `type` | Fields |
| --- | --- |
| `config` | `config` (same object as `--dry-run`) |
| `stage` | `stage`: `bridge_starting`, `bridge_ready`, `translating`, `validating` |
| `progress` | `stage`, optional `current`, `total`, `overall` (0-1); parsed best-effort from BabelDOC's console output |
| `warning` | `message` |
| `result` | `output_file`, `pages` |
| `error` | `code`, `message` |

Error codes: `executable_missing`, `node_version`, `input_missing`,
`input_invalid`, `glossary_missing`, `invalid_option`, `missing_api_key`,
`bridge_failed`, `babeldoc_failed`, `output_exists`, `output_not_found`,
`validation_failed`, `cancelled`, `error`. With `--dry-run`, only a single
`config` event is written. SIGTERM stops BabelDOC and the bridge cleanly and
exits with status 130 after a `cancelled` error event.

`translation_bridge.ts login --provider <name> --events json` writes prompts to
stdout (`select`, `secret`, `text`, `auth_url`, `device_code`, `info`, `done`,
`error`) and reads answers from stdin as `{"id":"p1","value":"..."}`. Secrets
are never echoed in events.

`translation_bridge.ts status --provider <name> [--auth-file ...] [--claude-bin ...]`
prints one `status` line with `installed`, `signed_in` (`null` for Claude Code,
which keeps its own login) and never prints credentials.
