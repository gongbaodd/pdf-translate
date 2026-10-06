#!/usr/bin/env node

import { randomBytes, randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { promises as fs } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";
import { createModels } from "@earendil-works/pi-ai";
import { openaiCodexProvider } from "@earendil-works/pi-ai/providers/openai-codex";
import { opencodeGoProvider } from "@earendil-works/pi-ai/providers/opencode-go";

const PROVIDERS = {
  codex: {
    id: "openai-codex",
    name: "Codex",
    aliasPrefix: "pi-codex-v1/",
    loginType: "oauth",
    defaultAuthFile: "babeldoc-codex",
    create: openaiCodexProvider,
  },
  "opencode-go": {
    id: "opencode-go",
    name: "OpenCode Go",
    aliasPrefix: "opencode-go/",
    loginType: "api_key",
    defaultAuthFile: "babeldoc-opencode-go",
    create: opencodeGoProvider,
  },
};
const MAX_BODY_BYTES = 4 * 1024 * 1024;
const LOCK_TIMEOUT_MS = 30_000;
const STALE_LOCK_MS = 5 * 60_000;

function parseArgs(argv) {
  const result = { _: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) {
      result._.push(arg);
      continue;
    }
    const equals = arg.indexOf("=");
    if (equals >= 0) {
      result[arg.slice(2, equals)] = arg.slice(equals + 1);
      continue;
    }
    const key = arg.slice(2);
    const value = argv[index + 1];
    if (value && !value.startsWith("--")) {
      result[key] = value;
      index += 1;
    } else {
      result[key] = true;
    }
  }
  return result;
}

function fail(message, exitCode = 1) {
  console.error(`translation_bridge: ${message}`);
  process.exitCode = exitCode;
}

function providerFrom(args) {
  const name = String(args.provider || "codex");
  const provider = PROVIDERS[name];
  if (!provider) {
    throw new Error(`unknown provider ${name}; choose one of: ${Object.keys(PROVIDERS).join(", ")}`);
  }
  return provider;
}

function authFileFrom(args) {
  const provider = providerFrom(args);
  const defaultFile = resolve(
    process.env.XDG_CONFIG_HOME || `${process.env.HOME || process.cwd()}/.config`,
    provider.defaultAuthFile,
    "auth.json",
  );
  return resolve(String(args["auth-file"] || defaultFile));
}

async function ensurePrivatePath(file) {
  const parent = dirname(file);
  await fs.mkdir(parent, { recursive: true, mode: 0o700 });
  await fs.chmod(parent, 0o700);
  try {
    const stat = await fs.lstat(file);
    if (stat.isSymbolicLink()) throw new Error(`refusing symlink credential file: ${file}`);
    if ((stat.mode & 0o077) !== 0) {
      throw new Error(`credential file is readable by group/others; run chmod 600 ${file}`);
    }
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}


async function acquireLock(file) {
  const lock = `${file}.lock`;
  const started = Date.now();
  while (Date.now() - started < LOCK_TIMEOUT_MS) {
    try {
      await fs.mkdir(lock, { mode: 0o700 });
      await fs.writeFile(`${lock}/owner`, `${process.pid}\n`, { mode: 0o600 });
      const heartbeat = setInterval(() => {
        void fs.utimes(lock, new Date(), new Date()).catch(() => {});
      }, 60_000);
      return async () => {
        clearInterval(heartbeat);
        await fs.rm(lock, { recursive: true, force: true });
      };
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      try {
        const stat = await fs.stat(lock);
        if (Date.now() - stat.mtimeMs > STALE_LOCK_MS) {
          await fs.rm(lock, { recursive: true, force: true });
          continue;
        }
      } catch (statError) {
        if (statError?.code !== "ENOENT") throw statError;
      }
      const wait = Promise.withResolvers();
      setTimeout(wait.resolve, 50 + Math.floor(Math.random() * 100));
      await wait.promise;
    }
  }
  throw new Error(`timed out waiting for credential lock: ${lock}`);
}

function validateCredential(value) {
  if (value === undefined) return undefined;
  if (!value || typeof value !== "object" || !["oauth", "api_key"].includes(value.type)) {
    throw new Error("credential store contains an invalid credential");
  }
  return value;
}

function createCredentialStore(file) {
  async function readAll() {
    await ensurePrivatePath(file);
    try {
      const text = await fs.readFile(file, "utf8");
      const value = JSON.parse(text);
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new Error("credential store must contain a JSON object");
      }
      return value;
    } catch (error) {
      if (error?.code === "ENOENT") return {};
      if (error instanceof SyntaxError) throw new Error(`invalid JSON in credential store: ${file}`);
      throw error;
    }
  }

  async function writeAll(value) {
    await ensurePrivatePath(file);
    const temporary = `${file}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`;
    await fs.writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
    await fs.chmod(temporary, 0o600);
    await fs.rename(temporary, file);
    await fs.chmod(file, 0o600);
  }

  return {
    async read(providerId) {
      return validateCredential((await readAll())[providerId]);
    },
    async list() {
      const all = await readAll();
      return Object.entries(all)
        .filter(([, credential]) => credential && typeof credential === "object")
        .map(([providerId, credential]) => ({ providerId, type: credential.type }));
    },
    async modify(providerId, fn) {
      await ensurePrivatePath(file);
      const release = await acquireLock(file);
      try {
        const all = await readAll();
        const current = validateCredential(all[providerId]);
        const next = validateCredential(await fn(current));
        if (next !== undefined) all[providerId] = next;
        await writeAll(all);
        return next;
      } finally {
        await release();
      }
    },
    async delete(providerId) {
      await ensurePrivatePath(file);
      const release = await acquireLock(file);
      try {
        const all = await readAll();
        delete all[providerId];
        await writeAll(all);
      } finally {
        await release();
      }
    },
  };
}

function textFromContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) throw new Error("message content must be a string or content array");
  return content
    .filter((part) => part?.type === "text")
    .map((part) => String(part.text ?? ""))
    .join("");
}

function requestText(messages) {
  if (!Array.isArray(messages) || messages.length === 0) {
    throw new Error("messages must be a non-empty array");
  }
  const systems = [];
  const users = [];
  for (const message of messages) {
    if (!message || typeof message !== "object") throw new Error("invalid message");
    const text = textFromContent(message.content);
    if (message.role === "system") systems.push(text);
    else if (message.role === "user") users.push(text);
    else throw new Error(`unsupported message role: ${String(message.role)}`);
  }
  if (users.length === 0) throw new Error("request must contain a user message");
  return { systemPrompt: systems.join("\n\n"), userText: users.join("\n\n") };
}

function openAIError(message, type, code) {
  return { error: { message, type, code } };
}

function usageFrom(response) {
  const usage = response?.usage || {};
  return {
    prompt_tokens: Number(usage.input || 0),
    completion_tokens: Number(usage.output || 0),
    total_tokens: Number(usage.totalTokens || (usage.input || 0) + (usage.output || 0)),
  };
}

function responseFrom(bridgeModel, response, text) {
  const finishReason = response.stopReason === "stop" ? "stop" : response.stopReason === "length" ? "length" : "error";
  return {
    id: `chatcmpl-${randomUUID()}`,
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model: bridgeModel,
    choices: [{ index: 0, message: { role: "assistant", content: text }, finish_reason: finishReason }],
    usage: usageFrom(response),
  };
}

function errorStatus(error) {
  const message = error instanceof Error ? error.message : String(error);
  if (/401|unauthori|credential|oauth|login/i.test(message)) return 401;
  if (/429|rate.?limit|quota|usage limit|too many/i.test(message)) return 429;
  if (/timeout|aborted|cancel/i.test(message)) return 504;
  return 502;
}

async function completeRequest(models, model, body, signal, sessionId) {
  if (body.stream === true) throw new Error("streaming is not supported by the BabelDOC bridge");
  if (body.tools || body.tool_choice || body.response_format) {
    throw new Error("tools, tool_choice, and response_format are not supported");
  }
  if (body.temperature !== undefined && body.temperature !== 0) {
    throw new Error("bridge only accepts BabelDOC's temperature=0 request");
  }
  if (body.max_tokens !== undefined && (!Number.isInteger(body.max_tokens) || body.max_tokens < 1)) {
    throw new Error("max_tokens must be a positive integer");
  }

  const { systemPrompt, userText } = requestText(body.messages);
  const context = {
    ...(systemPrompt ? { systemPrompt } : {}),
    messages: [{ role: "user", content: userText, timestamp: Date.now() }],
  };
  const options = {
    maxRetries: 0,
    transport: "sse",
    signal,
    ...(sessionId ? { sessionId } : {}),
    ...(body.max_tokens === undefined ? {} : { maxTokens: body.max_tokens }),
  };
  const response = await models.complete(model, context, options);
  if (!["stop", "length"].includes(response.stopReason)) {
    throw new Error(response.errorMessage || `completion ended with ${response.stopReason}`);
  }
  if (response.stopReason === "length") {
    throw new Error("completion was truncated by the output limit");
  }
  const text = response.content
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join("")
    .trim();
  if (!text) throw new Error("provider returned empty translated text");
  return responseFrom(body.model, response, text);
}

async function createRuntime(args) {
  const providerName = String(args.provider || "codex");
  const provider = providerFrom(args);
  const modelId = String(args.model || "");
  if (!modelId) throw new Error("--model is required");
  const authFile = authFileFrom(args);
  const credentials = createCredentialStore(authFile);
  const models = createModels({ credentials });
  models.setProvider(provider.create());
  const model = models.getModel(provider.id, modelId);
  if (!model) {
    const available = models.getModels(provider.id).map((entry) => entry.id).join(", ");
    throw new Error(`unknown ${provider.name} model ${modelId}; available pinned models: ${available || "none"}`);
  }
  const auth = await models.getAuth(model);
  if (!auth) {
    throw new Error(
      `no ${provider.name} credential found; run: npm run login -- --provider ${providerName} --auth-file ${authFile}`,
    );
  }
  return {
    models,
    model,
    modelId,
    authFile,
    provider,
    providerName,
    sessionId: randomUUID(),
  };
}


async function readJson(request) {
  let size = 0;
  const chunks = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) throw new Error("request body too large");
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw new Error("request body is not valid JSON");
  }
}

async function runServer(args) {
  const token = process.env.BABELDOC_BRIDGE_TOKEN;
  if (!token || token.length < 32) throw new Error("BABELDOC_BRIDGE_TOKEN must be a random local bearer token");
  const runtime = await createRuntime(args);
  const expectedAliases = new Set([runtime.modelId, `${runtime.provider.aliasPrefix}${runtime.modelId}`]);
  let active = 0;
  const queued = [];
  const server = createServer(async (request, response) => {
    const pathname = new URL(request.url || "/", "http://127.0.0.1").pathname;
    const write = (status, body) => {
      response.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
      response.end(JSON.stringify(body));
    };
    if (request.method === "GET" && pathname === "/healthz") {
      write(200, { status: "ok", provider: runtime.provider.id, model: runtime.modelId, active });
      return;
    }
    if (request.method !== "POST" || pathname !== "/v1/chat/completions") {
      write(404, openAIError("not found", "invalid_request_error", "not_found"));
      return;
    }
    const authorization = request.headers.authorization || "";
    if (authorization !== `Bearer ${token}`) {
      write(401, openAIError("invalid bridge bearer token", "authentication_error", "invalid_api_key"));
      return;
    }
    const slot = Promise.withResolvers();
    if (active === 0) {
      active = 1;
      slot.resolve();
    } else {
      queued.push(slot);
    }
    await slot.promise;
    const controller = new AbortController();
    request.once("aborted", () => controller.abort());
    try {
      const body = await readJson(request);
      if (!body || !expectedAliases.has(body.model)) throw new Error(`unsupported model alias: ${String(body?.model)}`);
      const result = await completeRequest(runtime.models, runtime.model, body, controller.signal, runtime.sessionId);
      write(200, result);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const status = error instanceof Error && /unsupported model alias|messages|content|role|temperature|max_tokens|not supported|JSON/.test(message) ? 400 : errorStatus(error);
      console.error(`translation_bridge: request failed with HTTP ${status}: ${message}`);
      write(status, openAIError(message, status === 429 ? "rate_limit_error" : status === 401 ? "authentication_error" : "api_error", "bridge_error"));
    } finally {
      const next = queued.shift();
      if (next) {
        next.resolve();
      } else {
        active = 0;
      }
    }
  });
  const listening = Promise.withResolvers();
  server.once("error", listening.reject);
  server.listen(Number(args.port || 0), "127.0.0.1", listening.resolve);
  await listening.promise;
  const address = server.address();
  process.stdout.write(`${JSON.stringify({ ready: true, port: address.port, model: runtime.modelId })}\n`);
  const shutdown = () => server.close(() => process.exit(0));
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
  const running = Promise.withResolvers();
  await running.promise;
}

async function promptSecret(rl, message) {
  const prompt = `${message}: `;
  if (!input.isTTY || typeof input.setRawMode !== "function") {
    return rl.question(prompt);
  }
  rl.pause();
  output.write(prompt);
  input.setRawMode(true);
  input.resume();
  const { promise, resolve: resolvePrompt, reject: rejectPrompt } = Promise.withResolvers();
  let value = "";
  const onData = (chunk) => {
    const key = String(chunk);
    if (key === "\u0003") {
      cleanup();
      rejectPrompt(new Error("login cancelled"));
    } else if (key === "\r" || key === "\n") {
      cleanup();
      output.write("\n");
      resolvePrompt(value);
    } else if (key === "\u007f" || key === "\b") {
      if (value) value = value.slice(0, -1);
    } else {
      value += key;
    }
  };
  const cleanup = () => {
    input.setRawMode(false);
    input.removeListener("data", onData);
    rl.resume();
  };
  input.on("data", onData);
  return promise;
}

async function runLogin(args) {
  const provider = providerFrom(args);
  const authFile = authFileFrom(args);
  const credentials = createCredentialStore(authFile);
  const models = createModels({ credentials });
  models.setProvider(provider.create());
  const rl = createInterface({ input, output });
  try {
    const credential = await models.login(provider.id, provider.loginType, {
      prompt: async (prompt) => {
        if (prompt.type === "select") {
          console.log(prompt.message);
          prompt.options.forEach((option, index) => console.log(`  ${index + 1}. ${option.label}`));
          const answer = await rl.question(`Choose 1-${prompt.options.length}: `);
          const selected = prompt.options[Number.parseInt(answer, 10) - 1];
          if (!selected) throw new Error("invalid login selection");
          return selected.id;
        }
        if (prompt.type === "secret") {
          return promptSecret(rl, prompt.message);
        }
        return rl.question(`${prompt.message}${prompt.placeholder ? ` (${prompt.placeholder})` : ""}: `);
      },
      notify: (event) => {
        if (event.type === "auth_url") {
          console.log(`\nOpen this URL in a browser:\n${event.url}`);
          if (event.instructions) console.log(event.instructions);
        } else if (event.type === "device_code") {
          console.log(`\nOpen ${event.verificationUri} and enter ${event.userCode}`);
        } else {
          console.log(event.message);
        }
      },
    }, { getDeviceId: randomUUID });
    console.log(`${provider.name} credentials saved to ${authFile}`);
    return credential;
  } finally {
    rl.close();
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args._[0] === "login") {
    await runLogin(args);
    return;
  }
  await runServer(args);
}

export { completeRequest, createCredentialStore, requestText };

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => fail(error instanceof Error ? error.message : String(error)));
}
