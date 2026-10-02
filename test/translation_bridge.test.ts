import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import test from "node:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { completeRequest, createCredentialStore, requestText } from "../scripts/translation_bridge.ts";

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
  }, new AbortController().signal);

  assert.equal(received.context.systemPrompt, "Translate only; preserve {{1}}.");
  assert.equal(received.context.messages[0].content, "bounded context {{1}}");
  assert.equal(received.options.maxTokens, 2048);
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
