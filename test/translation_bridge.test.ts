import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import test from "node:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough } from "node:stream";
import { writeFile } from "node:fs/promises";
import {
  claudeCodeArgs,
  claudeCodeEnvironment,
  completeClaudeCodeRequest,
  completeRequest,
  createCredentialStore,
  createJsonLoginIO,
  errorStatus,
  providerStatus,
  parseClaudeCodeResult,
  requestText,
  runClaudeCode,
} from "../scripts/translation_bridge.ts";

const model = { id: "gpt-5.6-luna" };

function response(stopReason: string, content: unknown[] = [{ type: "text", text: "限界上下文" }]) {
  return {
    stopReason,
    content,
    usage: { input: 12, output: 8, totalTokens: 20 },
  };
}

test("maps BabelDOC messages and returns a Chat Completions response", async () => {
  let received;
  const models = {
    complete: async (_model, context, options) => {
      received = { context, options };
      return response("stop");
    },
  };
  const result = await completeRequest(models, model, {
    model: "pi-codex-v1/gpt-5.6-luna",
    temperature: 0,
    max_tokens: 2048,
    messages: [
      { role: "system", content: "Translate only; preserve {{1}}." },
      { role: "user", content: "bounded context {{1}}" },
    ],
  }, new AbortController().signal, "bridge-session");

  assert.equal(received.context.systemPrompt, "Translate only; preserve {{1}}.");
  assert.equal(received.context.messages[0].content, "bounded context {{1}}");
  assert.equal(received.options.maxTokens, 2048);
  assert.equal(received.options.sessionId, "bridge-session");
  assert.equal("temperature" in received.options, false);
  assert.equal(result.object, "chat.completion");
  assert.equal(result.choices[0].message.content, "限界上下文");
  assert.deepEqual(result.usage, { prompt_tokens: 12, completion_tokens: 8, total_tokens: 20 });
});

test("rejects unsupported request features", async () => {
  await assert.rejects(
    completeRequest({ complete: async () => response("stop") }, model, {
      model: "pi-codex-v1/gpt-5.6-luna",
      stream: true,
      messages: [{ role: "user", content: "translate" }],
    }, new AbortController().signal),
    /streaming is not supported/,
  );
  assert.throws(
    () => requestText([{ role: "assistant", content: "not a request" }]),
    /unsupported message role/,
  );
});

test("rejects truncated and empty provider responses", async () => {
  await assert.rejects(
    completeRequest({ complete: async () => response("length") }, model, {
      model: "pi-codex-v1/gpt-5.6-luna",
      messages: [{ role: "user", content: "translate" }],
    }, new AbortController().signal),
    /truncated/,
  );
  await assert.rejects(
    completeRequest({ complete: async () => response("stop", []) }, model, {
      model: "pi-codex-v1/gpt-5.6-luna",
      messages: [{ role: "user", content: "translate" }],
    }, new AbortController().signal),
    /empty translated text/,
  );
});

test("creates and protects a nested credential store before locking", async () => {
  const directory = await mkdtemp(join(tmpdir(), "credential-store-test-"));
  const file = join(directory, "nested", "auth.json");
  try {
    const store = createCredentialStore(file);
    const credential = { type: "oauth", refresh: "refresh", access: "access", expires: 4102444800000 };
    await store.modify("openai-codex", async () => credential);
    assert.deepEqual(JSON.parse(await readFile(file, "utf8")), { "openai-codex": credential });
    assert.equal((await stat(file)).mode & 0o077, 0);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

function claudeResult(overrides = {}) {
  return JSON.stringify({
    type: "result",
    subtype: "success",
    is_error: false,
    result: "限界上下文",
    stop_reason: "end_turn",
    usage: { input_tokens: 2, cache_creation_input_tokens: 10, cache_read_input_tokens: 30, output_tokens: 5 },
    ...overrides,
  });
}

const claudeRuntime = { command: "claude", modelId: "sonnet", timeoutMs: 60_000, cwd: "/tmp" };

test("claude-code backend runs the CLI without tools and maps the result", async () => {
  let received;
  const previous = process.env.ANTHROPIC_API_KEY;
  process.env.ANTHROPIC_API_KEY = "sk-should-not-leak";
  try {
    const result = await completeClaudeCodeRequest(claudeRuntime, {
      model: "claude-code/sonnet",
      temperature: 0,
      max_tokens: 2048,
      messages: [
        { role: "system", content: "Translate only; preserve {{1}}." },
        { role: "user", content: "bounded context {{1}}" },
      ],
    }, new AbortController().signal, async (options) => {
      received = options;
      return { code: 0, stdout: claudeResult(), stderr: "" };
    });

    assert.equal(received.command, "claude");
    assert.equal(received.input, "bounded context {{1}}");
    assert.equal(received.cwd, "/tmp");
    const args = received.args;
    assert.equal(args[args.indexOf("--model") + 1], "sonnet");
    assert.equal(args[args.indexOf("--tools") + 1], "");
    assert.equal(args[args.indexOf("--output-format") + 1], "json");
    assert.match(args[args.indexOf("--system-prompt") + 1], /Translate only; preserve \{\{1\}\}\.$/);
    assert.ok(args.includes("--no-session-persistence"));
    assert.equal("ANTHROPIC_API_KEY" in received.env, false);
    assert.equal(result.object, "chat.completion");
    assert.equal(result.model, "claude-code/sonnet");
    assert.equal(result.choices[0].message.content, "限界上下文");
    assert.deepEqual(result.usage, { prompt_tokens: 42, completion_tokens: 5, total_tokens: 47 });
  } finally {
    if (previous === undefined) delete process.env.ANTHROPIC_API_KEY;
    else process.env.ANTHROPIC_API_KEY = previous;
  }
});

test("claude-code backend surfaces CLI errors for status mapping", () => {
  assert.throws(
    () => parseClaudeCodeResult({
      code: 1,
      stdout: claudeResult({ subtype: "success", is_error: true, result: "Claude AI usage limit reached" }),
      stderr: "",
    }),
    /Claude Code CLI error: Claude AI usage limit reached/,
  );
  assert.throws(
    () => parseClaudeCodeResult({ code: 1, stdout: "", stderr: "Invalid API key · Please run /login" }),
    /status 1: Invalid API key · Please run \/login/,
  );
  assert.throws(
    () => parseClaudeCodeResult({ code: 0, stdout: claudeResult({ result: "  " }), stderr: "" }),
    /empty translated text/,
  );
  assert.throws(
    () => parseClaudeCodeResult({ code: 0, stdout: claudeResult({ stop_reason: "max_tokens" }), stderr: "" }),
    /truncated/,
  );
});

test("claude-code environment drops API billing variables and the bridge token", () => {
  const env = claudeCodeEnvironment({
    PATH: "/usr/bin",
    ANTHROPIC_API_KEY: "a",
    ANTHROPIC_AUTH_TOKEN: "b",
    BABELDOC_BRIDGE_TOKEN: "c",
    CLAUDE_CODE_OAUTH_TOKEN: "subscription",
  });
  assert.deepEqual(env, { PATH: "/usr/bin", CLAUDE_CODE_OAUTH_TOKEN: "subscription" });
  assert.equal(claudeCodeArgs("haiku", "").at(claudeCodeArgs("haiku", "").indexOf("--model") + 1), "haiku");
});

test("runClaudeCode feeds stdin to the process and honors aborts", async () => {
  const echoed = await runClaudeCode({
    command: process.execPath,
    args: ["-e", "process.stdin.pipe(process.stdout)"],
    input: "hello",
    timeoutMs: 10_000,
    cwd: process.cwd(),
    env: process.env,
  });
  assert.deepEqual(echoed, { code: 0, stdout: "hello", stderr: "" });

  const controller = new AbortController();
  const pending = runClaudeCode({
    command: process.execPath,
    args: ["-e", "setTimeout(() => {}, 60_000)"],
    input: "",
    signal: controller.signal,
    timeoutMs: 30_000,
    cwd: process.cwd(),
    env: process.env,
  });
  controller.abort();
  await assert.rejects(pending, /aborted/);

  await assert.rejects(
    runClaudeCode({ command: "definitely-not-a-claude-binary", args: [], input: "", timeoutMs: 5_000, cwd: process.cwd(), env: process.env }),
    /executable not found/,
  );
});

test("maps Claude subscription limit messages to HTTP 429", () => {
  assert.equal(errorStatus(new Error("Claude Code CLI error: You've hit your 5-hour limit · resets 7:50pm (Europe/Tallinn)")), 429);
  assert.equal(errorStatus(new Error("Claude Code CLI error: Claude AI usage limit reached")), 429);
  assert.equal(errorStatus(new Error("Claude Code CLI exited with status 1: Error: No messages returned from query")), 502);
});

function collectLines(stream: PassThrough) {
  const lines: any[] = [];
  let buffer = "";
  stream.on("data", (chunk) => {
    buffer += chunk.toString();
    let index;
    while ((index = buffer.indexOf("\n")) >= 0) {
      lines.push(JSON.parse(buffer.slice(0, index)));
      buffer = buffer.slice(index + 1);
    }
  });
  return lines;
}

const tick = () => new Promise((resolve) => setImmediate(resolve));

test("JSON login IO relays prompts and answers without echoing secrets", async () => {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  const lines = collectLines(stdout);
  const io = createJsonLoginIO({ stdin, stdout });

  const secret = io.prompt({ type: "secret", message: "API key" });
  await tick();
  assert.deepEqual(lines[0], { v: 1, type: "secret", message: "API key", id: "p1" });
  stdin.write(`${JSON.stringify({ id: "p1", value: "sk-test-secret" })}\n`);
  assert.equal(await secret, "sk-test-secret");

  const select = io.prompt({ type: "select", message: "Pick", options: [{ id: "a", label: "A" }] });
  await tick();
  assert.equal(lines[1].type, "select");
  stdin.write(`${JSON.stringify({ id: "p2", value: "zzz" })}\n`);
  await assert.rejects(select, /invalid login selection/);

  io.notify({ type: "auth_url", url: "https://example.test/auth", instructions: "go" });
  io.notify({ type: "device_code", verificationUri: "https://example.test/device", userCode: "ABCD" });
  assert.equal(lines[2].type, "auth_url");
  assert.deepEqual(lines[3], {
    v: 1,
    type: "device_code",
    verification_uri: "https://example.test/device",
    user_code: "ABCD",
  });
  assert.ok(!JSON.stringify(lines).includes("sk-test-secret"));
  io.close();
});

test("JSON login IO rejects pending prompts when stdin closes", async () => {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  collectLines(stdout);
  const io = createJsonLoginIO({ stdin, stdout });
  const pending = io.prompt({ type: "text", message: "Code" });
  await tick();
  stdin.end();
  await assert.rejects(pending, /login cancelled/);
});

test("provider status reports credentials without exposing them", async () => {
  const directory = await mkdtemp(join(tmpdir(), "bridge-status-"));
  try {
    const authFile = join(directory, "auth.json");
    const args = { provider: "opencode-go", "auth-file": authFile };
    let status = await providerStatus(args);
    assert.equal(status.signed_in, false);

    await writeFile(authFile, JSON.stringify({ "opencode-go": { type: "api_key", key: "sk-hidden" } }), { mode: 0o600 });
    status = await providerStatus(args);
    assert.equal(status.signed_in, true);
    assert.equal(status.credential_type, "api_key");
    assert.ok(!JSON.stringify(status).includes("sk-hidden"));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("claude-code status checks the CLI is installed", async () => {
  const ok = await providerStatus(
    { provider: "claude-code" },
    { run: async () => ({ code: 0, stdout: "2.1.0 (Claude Code)\n", stderr: "" }) },
  );
  assert.equal(ok.installed, true);
  assert.equal(ok.version, "2.1.0 (Claude Code)");
  const missing = await providerStatus(
    { provider: "claude-code" },
    { run: async () => { throw new Error("executable not found: claude"); } },
  );
  assert.equal(missing.installed, false);
});
