# BabelDOC bilingual PDF translation plan

## Goal

Write a Python wrapper around BabelDOC's CLI, with a local TypeScript bridge
using pi-ai, to translate `learningdomain-drivendesign.pdf` with either Codex
OAuth or an OpenCode Go API key into a bilingual PDF with English on the left
and Simplified Chinese on the right. Each original page and its translation
should share one wide output page. Retain direct OpenAI-compatible API access
as an explicitly selected alternative.
BabelDOC supports this layout directly. Keep its CLI as the integration boundary
instead of coupling the wrapper to its internal Python APIs. The CLI is documented
as primarily for debugging and has no end-user technical support; pin its version
and verify compatibility with a sample before translating the book.

## Translation backend design

Use the provider/auth/model separation in
[pi's AI package](https://github.com/earendil-works/pi/tree/main/packages/ai).
Reuse `@earendil-works/pi-ai` rather than reimplementing provider
authentication, account headers, or the provider transports in Python.

```text
scripts/translate_pdf.py
  -> BabelDOC CLI (PDF extraction, translation prompts, cache, layout)
     -> local /v1/chat/completions bridge
        -> pi-ai Models collection
           -> OpenAI Codex provider (OAuth / ChatGPT subscription)
           -> OpenCode Go provider (API key / Go subscription)
```

- BabelDOC's OpenAI translator calls Chat Completions. pi-ai's subscription
  providers use provider-specific transports, so setting BabelDOC's base URL to
  an upstream provider alone will not work.
- Default wrapper backend: `codex`, using `openaiCodexProvider()` and an
  explicitly selected model from that provider's catalog. `opencode-go` is an
  equivalent bridge backend using `opencodeGoProvider()` and its catalog.
- Neither subscription backend requires `OPENAI_API_KEY`; OpenCode Go requires
  an API key obtained from the OpenCode Console.
- Alternative wrapper backend: `openai-compatible`, using BabelDOC directly
  with the configured endpoint, model, and API key; no bridge or subscription
  login. Never silently switch from subscription authentication to paid API
  billing.
- Pin a release/commit that includes both selected providers and model catalogs.
  Any later provider migration must explicitly revalidate authentication,
  model availability, session headers, and transport.

Design references:
- [pi-ai providers, Models, and authentication](https://github.com/earendil-works/pi/blob/main/packages/ai/README.md)
- [OpenCode Go provider documentation](https://opencode.ai/docs/go/)
- [BabelDOC CLI configuration](https://funstory-ai.github.io/BabelDOC/#advanced-options)
- [BabelDOC Chat Completions translator](https://github.com/funstory-ai/BabelDOC/blob/main/babeldoc/translator/translator.py)


## Source inspection

- Input: `learningdomain-drivendesign.pdf` in the project root.
- 342 pages, approximately 16 MB, unencrypted.
- All pages measure approximately 504 × 662 points.
- Sampled body pages contain extractable text, including code.
- The cover has no extractable text; image-only text needs separate review.

## Implementation steps

### 1. Set up a reproducible environment

- Install `uv`, Python 3.12, and a pinned BabelDOC version.
- Pin a compatible Node.js runtime and `@earendil-works/pi-ai`, with a lockfile
  and a TypeScript build/run command for the bridge. Record the upstream revision
  used to verify the provider API; do not run an unpinned `latest` dependency.
- Download the required fonts and layout models using `babeldoc --warmup`.
- Record installation, OAuth login, and usage instructions for both backends.

### 2. Write `scripts/translate_pdf.py`

- Default to this book while accepting another input path.
- Support `--backend codex|opencode-go|openai-compatible` (default `codex`),
  `--pages`, `--output-dir`, `--model`, `--base-url`, `--qps`, `--auth-file`,
  and `--dry-run`. Require an explicit model and validate it against the
  selected pinned provider catalog.
- Treat `--base-url` as a direct API-backend option only. In either bridge
  backend, allocate a loopback port and configure BabelDOC to use the bridge's
  `/v1` base URL, `--openai`, and a provider-qualified model alias.
- In direct API mode, read the API key from an environment variable. In bridge
  modes, only the bridge reads the provider credential store. Keep all
  credentials out of logs, command-line arguments, and committed files; use a
  restricted temporary BabelDOC config when a credential cannot be supplied via
  environment.
- Start one bridge per translation run, wait for readiness before starting
  BabelDOC, and stop both children on completion, failure, or interruption.
- Validate dependencies, input, model, and the selected authentication route
  before translation. Provide a separate interactive login command; never
  initiate browser login unexpectedly during a long-running book translation.
- Make `--dry-run` print the redacted resolved configuration and commands without
  login, token refresh, bridge startup, downloads, or translation requests.

### 3. Implement `scripts/translation_bridge.ts`

#### Provider and authentication

- Register both `openaiCodexProvider()` and `opencodeGoProvider()` in
  `createModels({ credentials })`; select the model with the matching provider
  ID. Use the public Models login and completion methods, not private auth
  implementation imports.
- Implement provider-selected login around
  `models.login("openai-codex", "oauth", ...)` for Codex and
  `models.login("opencode-go", "api_key", ...)` for OpenCode Go. Let pi-ai own
  OAuth, API-key prompting, and provider-specific request authentication.
- Inject a persistent `CredentialStore` outside the repository, selected by
  `--auth-file`; restrict the directory/file to the current user. Implement its
  serialized `modify` contract with process-safe locking and atomic writes so
  concurrent requests/runs cannot race refresh-token rotation.
- Let Models refresh expiring Codex credentials and persist the refreshed values.
  Missing/revoked credentials must produce an actionable re-login error, not an
  empty translation or API-key fallback. OpenCode Go uses its stored API key
  without OAuth refresh.
- Bind only to `127.0.0.1`, require a per-run random bridge bearer token, and pass
  only that local token to BabelDOC. Never expose provider credentials to
  BabelDOC or forward the local bearer token upstream.

#### BabelDOC compatibility boundary

- Implement the non-streaming `POST /v1/chat/completions` subset used by the
  pinned BabelDOC translator, plus a local readiness endpoint. This is a
  translation adapter, not a general-purpose OpenAI proxy.
- Map system instructions to pi-ai's `Context.systemPrompt` and user text to
  its message format; preserve BabelDOC's glossary, rich-text/formula
  placeholders, and translation-only instructions unchanged. Use a fresh
  context per request, with no tools or conversation history shared across
  paragraphs.
- Use `models.complete()` or `models.completeSimple()` and collect only final
  text blocks. pi-ai may stream internally; return one Chat Completions response
  with `choices[0].message.content`, an appropriate finish reason, and mapped
  token usage. Never include reasoning blocks in translated text.
- Explicitly handle BabelDOC's `temperature` and `max_tokens` fields according
  to the selected provider's capabilities; do not blindly forward unsupported
  parameters. Document any unsupported sampling/output-limit control and reject
  unsupported request shapes. Keep `--enable-json-mode-if-requested` off unless
  JSON-mode compatibility has been implemented and verified.
- Check completion stop reasons, not just thrown exceptions: authentication,
  rate-limit, transport, cancellation, empty output, and truncated responses
  must not become successful partial/empty translations. Return actionable
  OpenAI-shaped HTTP errors, preserving rate-limit semantics.
- Bound in-flight subscription-backend requests to one initially, with BabelDOC
  QPS 1 and `--pool-max-workers 1 --term-pool-max-workers 1`. Measure the
  sample before increasing concurrency; QPS alone does not bound long-running
  requests.
  Account for retries already performed by BabelDOC and pi-ai rather than
  adding another independent retry loop.
- Preserve BabelDOC caching. Use a stable provider/model-qualified alias,
  resolved by the bridge, to separate each subscription backend from direct-API
  cache entries. Send a stable `x-opencode-session` value for OpenCode Go.
  Version the alias when translation-affecting bridge settings change; never
  include credentials or the ephemeral port in the cache identity.

### 4. Configure bilingual output

- Use `--lang-in en --lang-out zh-CN --no-mono`.
- Keep side-by-side mode with original text first: do not enable
  `--use-alternating-pages-dual` or `--dual-translate-first`.
- Use `--watermark-output-mode no_watermark`.
- Process in 25-page parts using `--max-pages-per-part 25`, with automatic
  merging. Start either subscription backend at QPS 1 and one worker as above;
  direct API mode may retain the original conservative QPS of 2.

Configuration reference: https://funstory-ai.github.io/BabelDOC/#advanced-options

### 5. Add consistent DDD terminology

- Supply a glossary CSV through `--glossary-files`, with `source`, `target`,
  and optional `tgt_lng` columns, covering terms such as:
  - bounded context → 限界上下文
  - aggregate → 聚合
  - ubiquitous language → 通用语言
- Check that code identifiers and syntax remain intact.
- Flag image-only text for review rather than assuming it was translated.

### 6. Verify the bridge, then translate a small sample

- First send a real translation request through the local Chat Completions
  endpoint using the selected Codex model and OAuth credentials. Confirm usable
  Chinese text, preserved placeholders/code, and a BabelDOC-compatible response.
- Cover expired-token refresh, concurrent refresh serialization, missing/revoked
  credentials, unauthorized local requests, rate limits, truncated responses,
  and cancellation with focused adapter tests. Prove an actual BabelDOC request
  succeeds; mock-provider tests alone do not establish Codex compatibility.
- Start with physical PDF pages 31, 81, and 151, covering prose, a diagram,
  and code. These are PDF page positions, not printed book page numbers.
- Check Chinese glyphs, clipping, terminology, code preservation, and
  left/right alignment.
- Use the sample to assess quality and runtime before the full run.

### 7. Run and validate the complete book

- Preserve translation caching, capture progress, and report failures clearly.
- Confirm 342 paired output pages, approximately 1008 × 662 points per page,
  and Chinese text on translated body pages.
- Visually inspect representative pages throughout the book.
- Save the final output as:
  `output/pdf/learningdomain-drivendesign.en-zh.side-by-side.pdf`.

## Required configuration before execution

| Backend | Required configuration |
| --- | --- |
| `codex` (default) | Pinned pi-ai runtime/provider, a ChatGPT account entitled to the selected Codex model, completed OAuth login, private writable credential store, and explicit model ID. The local bridge endpoint/token are managed by the wrapper. |
| `openai-compatible` | Explicit endpoint, model ID, and API-key environment variable. No Codex credentials or bridge required. |

An existing Codex subscription is not an OpenAI API key. Confirm account access
and usage limits with the live sample before committing to the full book.
Book text is sent to the selected upstream service in either mode.

## Acceptance criteria

- Codex mode translates the sample end to end through BabelDOC and pi-ai without
  requiring `OPENAI_API_KEY` or invoking the Codex coding-agent CLI.
- OAuth persists across runs and refreshes safely; failures do not silently
  switch providers or contaminate the translation cache with partial results.
- Direct OpenAI-compatible mode remains usable without starting the bridge.
- Dry runs perform no network/auth side effects and disclose no credentials.
- Sample and full-book outputs meet the layout, terminology, code-preservation,
  and page-count checks above; interruption leaves no orphan bridge process.

## Status

Implementation complete for the repository tooling:

- `pyproject.toml` pins BabelDOC 0.6.4 for Python 3.12.
- `package.json` and `package-lock.json` pin `@earendil-works/pi-ai` 1.0.0.
- `scripts/translate_pdf.py` implements Codex and direct OpenAI-compatible modes.
- `scripts/translation_bridge.ts` implements the loopback Chat Completions adapter,
  OAuth credential storage, refresh-safe file locking, and interactive login.
- `test/translation_bridge.test.ts` covers message mapping, unsupported requests,
  Codex option compatibility, credential-store initialization, truncation, and
  empty-response failures without network access.
- The bridge path was verified with `gpt-6-luna`; a minimal request returned
  Simplified Chinese after omitting BabelDOC's unsupported `temperature` field
  from the Codex Responses request.
- Page-limited runs disable BabelDOC part splitting because BabelDOC 0.6.4
  asserts on empty split parts when selected pages are outside the first part;
  the resulting dual PDF retains the source document's 342-page output and
  validates the selected translated pages.
- `scripts/translate_pdf.py --debug` preserves BabelDOC tracebacks and debug
  artifacts for failures before PDF generation.
- The three-page sample completed successfully with `gpt-6-luna`; the output
  was written under `output/sample`.
- Custom positional PDF inputs are supported; final output names derive from the
  input stem, while the default book retains its planned output filename.
- `glossary/ddd-en-zh.csv` and `docs/translation.md` provide the terminology and
  operating instructions.

The full-book translation has not been completed. It remains an execution gate
because it requires the user's account, network access, model entitlement, and
full PDF inspection.
