// worker/src/main.ts
import { existsSync as existsSync6 } from "node:fs";
import { dirname, join as join7 } from "node:path";
import { fileURLToPath } from "node:url";

// worker/src/core.ts
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { WASI } from "node:wasi";
var CoreError = class extends Error {
  op;
  constructor(op, message) {
    super(message);
    this.name = "CoreError";
    this.op = op;
  }
};
var WA = globalThis.WebAssembly;
var encoder = new TextEncoder();
var decoder = new TextDecoder();
var ROOT = process.platform === "win32" ? winDrives() : { "/": "/" };
function winDrives() {
  const out = {};
  for (let c = 65; c <= 90; c++) {
    const root = `${String.fromCharCode(c)}:\\`;
    if (existsSync(root)) out[`/${String.fromCharCode(c).toLowerCase()}`] = root;
  }
  return out;
}
function toGuestPath(s) {
  const m = /^([A-Za-z]):(?:[\\/](.*))?$/.exec(s);
  if (!m) return s;
  const rest = (m[2] ?? "").replace(/\\/g, "/");
  return rest ? `/${m[1].toLowerCase()}/${rest}` : `/${m[1].toLowerCase()}`;
}
function toHostPath(s) {
  const m = /^\/([a-z])(?:\/(.*))?$/.exec(s);
  if (!m) return s;
  const rest = (m[2] ?? "").replace(/\//g, "\\");
  return `${m[1].toUpperCase()}:\\${rest}`;
}
function mapPaths(v, fn) {
  if (typeof v === "string") return fn(v);
  if (Array.isArray(v)) return v.map((x) => mapPaths(x, fn));
  if (v && typeof v === "object") {
    const out = {};
    for (const [k, val] of Object.entries(v)) out[k] = mapPaths(val, fn);
    return out;
  }
  return v;
}
function mapping(preopens) {
  const out = {};
  if (Array.isArray(preopens)) {
    for (const p of preopens) out[process.platform === "win32" ? toGuestPath(p) : p] = p;
  } else Object.assign(out, preopens);
  for (const guest of Object.keys(out)) if (!existsSync(out[guest])) delete out[guest];
  return out;
}
function patchWinReaddir(preview1, preopens, getBuf) {
  const fdPaths = /* @__PURE__ */ new Map();
  const origPrestat = preview1.fd_prestat_dir_name;
  const origOpen = preview1.path_open;
  const origClose = preview1.fd_close;
  const origReaddir = preview1.fd_readdir;
  preview1.fd_prestat_dir_name = (fd, pathPtr, pathLen) => {
    const r = origPrestat(fd, pathPtr, pathLen);
    const mem = getBuf();
    if (r === 0 && mem) {
      const guest = decoder.decode(new Uint8Array(mem, Number(pathPtr), Number(pathLen)).slice()).replace(/\0+$/, "");
      const host = preopens[guest] ?? preopens[`/${guest}`];
      if (host) fdPaths.set(Number(fd), host);
    }
    return r;
  };
  preview1.path_open = (...args) => {
    const r = origOpen(...args);
    const mem = getBuf();
    if (r === 0 && mem) {
      const dirfd = Number(args[0]);
      const pathPtr = Number(args[2]);
      const pathLen = Number(args[3]);
      const outFdPtr = Number(args[8]);
      const base = fdPaths.get(dirfd);
      if (base) {
        const rel = decoder.decode(new Uint8Array(mem, pathPtr, pathLen).slice());
        const openedFd = new DataView(mem).getUint32(outFdPtr, true);
        fdPaths.set(openedFd, join(base, rel));
      }
    }
    return r;
  };
  preview1.fd_close = (fd) => {
    fdPaths.delete(Number(fd));
    return origClose(fd);
  };
  preview1.fd_readdir = (fd, bufPtr, bufLen, cookie, bufusedPtr) => {
    const r = origReaddir(fd, bufPtr, bufLen, cookie, bufusedPtr);
    if (r !== 52) return r;
    const hostDir = fdPaths.get(Number(fd));
    const mem = getBuf();
    if (!hostDir || !mem) return r;
    let entries;
    try {
      entries = readdirSync(hostDir, { withFileTypes: true });
    } catch {
      return 8;
    }
    const view = new DataView(mem);
    const bytes = new Uint8Array(mem);
    const start = Number(cookie);
    const cap = Number(bufLen);
    const base = Number(bufPtr);
    let off = 0;
    for (let i = start; i < entries.length; i++) {
      const e = entries[i];
      const nameBytes = encoder.encode(e.name);
      const recLen = 24 + nameBytes.length;
      if (off + recLen > cap) {
        if (off === 0) {
          const take = Math.min(cap, recLen);
          const tmp = new Uint8Array(recLen);
          const tv = new DataView(tmp.buffer);
          tv.setBigUint64(0, BigInt(i + 1), true);
          tv.setBigUint64(8, BigInt(i + 1), true);
          tv.setUint32(16, nameBytes.length, true);
          tmp[20] = e.isDirectory() ? 3 : e.isFile() ? 4 : 0;
          tmp.set(nameBytes, 24);
          bytes.set(tmp.subarray(0, take), base);
          off = take;
        }
        break;
      }
      view.setBigUint64(base + off, BigInt(i + 1), true);
      view.setBigUint64(base + off + 8, BigInt(i + 1), true);
      view.setUint32(base + off + 16, nameBytes.length, true);
      bytes[base + off + 20] = e.isDirectory() ? 3 : e.isFile() ? 4 : 0;
      bytes[base + off + 21] = 0;
      bytes[base + off + 22] = 0;
      bytes[base + off + 23] = 0;
      bytes.set(nameBytes, base + off + 24);
      off += recLen;
    }
    view.setUint32(Number(bufusedPtr), off, true);
    return 0;
  };
}
var Core = class _Core {
  module;
  preopens;
  exports = null;
  busy = false;
  constructor(module, preopens) {
    this.module = module;
    this.preopens = preopens;
  }
  /** Compiles the wasm at `wasmPath`; the preopened folders are what the core may touch. */
  static async load(wasmPath, preopens = ROOT) {
    const module = await WA.compile(readFileSync(wasmPath));
    const core = new _Core(module, mapping(preopens));
    core.instance();
    return core;
  }
  /** The folders the core can reach (guest path → host path). */
  get folders() {
    return this.preopens;
  }
  instance() {
    if (this.exports) return this.exports;
    const wasi = new WASI({ version: "preview1", args: ["atrium-core"], env: {}, preopens: this.preopens, returnOnExit: true });
    const imports = wasi.getImportObject();
    let exportsRef = null;
    if (process.platform === "win32") patchWinReaddir(imports.wasi_snapshot_preview1, this.preopens, () => exportsRef?.memory.buffer ?? null);
    const instance = new WA.Instance(this.module, imports);
    exportsRef = instance.exports;
    wasi.initialize(instance);
    this.exports = exportsRef;
    return this.exports;
  }
  /**
   * Runs one op. Calls never overlap: the core is synchronous, and a call made from inside
   * another (there is no way to) is refused. A crash inside the core (a trap) throws and the
   * next call starts a fresh instance; all state is in files, so nothing is lost.
   */
  call(op, args = {}) {
    if (this.busy) throw new CoreError(op, "the core is busy");
    this.busy = true;
    try {
      return this.run(op, args);
    } finally {
      this.busy = false;
    }
  }
  run(op, args) {
    const wasm = this.instance();
    const payload = process.platform === "win32" ? mapPaths(args ?? {}, toGuestPath) : args ?? {};
    const request = encoder.encode(JSON.stringify({ op, args: payload }));
    let reply;
    try {
      const ptr = wasm.alloc(request.length);
      new Uint8Array(wasm.memory.buffer, ptr, request.length).set(request);
      const packed = wasm.call(ptr, request.length);
      wasm.dealloc(ptr, request.length);
      const out = Number(packed >> 32n);
      const len = Number(packed & 0xffffffffn);
      reply = decoder.decode(new Uint8Array(wasm.memory.buffer, out, len).slice());
      wasm.dealloc(out, len);
    } catch (error) {
      this.exports = null;
      throw new CoreError(op, `the core crashed: ${error instanceof Error ? error.message : String(error)}`);
    }
    const parsed = JSON.parse(reply);
    if (typeof parsed.err === "string") throw new CoreError(op, parsed.err);
    return process.platform === "win32" ? mapPaths(parsed.ok, toHostPath) : parsed.ok;
  }
};

// worker/src/host.ts
function orcaHost(orca) {
  return {
    call: (method, params) => orca.host.call(method, params ?? {}),
    has: (method) => {
      try {
        return orca.host.has(method);
      } catch {
        return false;
      }
    },
    on: (event, handler) => orca.events.on(event, handler),
    hook: (name, handler) => orca.hooks.on(name, handler),
    handle: (name, handler) => orca.panels.handle(name, (input) => handler(input)),
    post: (name, payload) => {
      try {
        orca.panels.post(name, payload ?? null);
      } catch (error) {
        orca.log(`post ${name}: ${String(error)}`);
      }
    },
    command: (id, handler) => orca.commands.register(id, (args) => handler(args)),
    storageGet: async (key2) => await orca.storage.get(key2) ?? null,
    storageSet: (key2, value) => orca.storage.set(key2, value),
    settings: async () => (await orca.host.call("settings.get", {}))?.settings ?? {},
    onSettings: (listener) => orca.settings?.onDidChange?.(() => listener()) ?? { dispose() {
    } },
    log: (message) => orca.log(message)
  };
}
var HostError = class extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
};

// worker/src/bridge/server.ts
import { createHash } from "node:crypto";
import { chmodSync, existsSync as existsSync2, mkdirSync, unlinkSync } from "node:fs";
import net from "node:net";
import { tmpdir, userInfo } from "node:os";
import { join as join2 } from "node:path";
function socketPath(seed, platform = process.platform) {
  const user = safeUser();
  const tag = createHash("sha256").update(`${user}:${seed}`).digest("hex").slice(0, 12);
  if (platform === "win32") return `\\\\.\\pipe\\atrium-orca-${tag}`;
  const base = process.env.XDG_RUNTIME_DIR && existsSync2(process.env.XDG_RUNTIME_DIR) ? process.env.XDG_RUNTIME_DIR : tmpdir();
  return join2(base, `atrium-orca-${tag}.sock`);
}
function safeUser() {
  try {
    return userInfo().username;
  } catch {
    return "user";
  }
}
var MAX_LINE = 1024 * 1024;
var BridgeServer = class {
  constructor(path, token, handler, log = () => {
  }) {
    this.path = path;
    this.token = token;
    this.handler = handler;
    this.log = log;
  }
  server = null;
  async start() {
    if (process.platform !== "win32") {
      try {
        mkdirSync(join2(this.path, ".."), { recursive: true });
      } catch {
      }
      if (existsSync2(this.path)) {
        try {
          unlinkSync(this.path);
        } catch {
        }
      }
    }
    const server = net.createServer((socket) => this.serve(socket));
    await new Promise((resolve3, reject) => {
      server.once("error", reject);
      server.listen(this.path, () => {
        server.off("error", reject);
        resolve3();
      });
    });
    server.on("error", (error) => this.log(`bridge: ${String(error)}`));
    if (process.platform !== "win32") {
      try {
        chmodSync(this.path, 384);
      } catch {
      }
    }
    this.server = server;
  }
  async stop() {
    const server = this.server;
    this.server = null;
    if (!server) return;
    await new Promise((resolve3) => server.close(() => resolve3()));
    if (process.platform !== "win32" && existsSync2(this.path)) {
      try {
        unlinkSync(this.path);
      } catch {
      }
    }
  }
  serve(socket) {
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("error", () => {
    });
    socket.on("data", (chunk) => {
      buffer += chunk;
      if (buffer.length > MAX_LINE) {
        this.reply(socket, { ok: false, error: "request too large" });
        socket.end();
        return;
      }
      let at;
      while ((at = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, at);
        buffer = buffer.slice(at + 1);
        if (line.trim()) void this.answer(socket, line);
      }
    });
  }
  async answer(socket, line) {
    let request;
    try {
      request = JSON.parse(line);
    } catch {
      this.reply(socket, { ok: false, error: "not JSON" });
      return;
    }
    if (request.token !== this.token) {
      this.reply(socket, { ok: false, error: "this terminal was not started by Atrium (bad token)" });
      return;
    }
    if (typeof request.cmd !== "string" || !Array.isArray(request.args)) {
      this.reply(socket, { ok: false, error: "bad request" });
      return;
    }
    try {
      this.reply(socket, await this.handler(request));
    } catch (error) {
      this.reply(socket, { ok: false, error: error instanceof Error ? error.message : String(error) });
    }
  }
  reply(socket, reply) {
    if (!socket.destroyed) socket.write(`${JSON.stringify(reply)}
`);
  }
};

// worker/src/service.ts
import { homedir } from "node:os";
import { join as join6 } from "node:path";

// worker/src/bridge/hook.ts
import { delimiter } from "node:path";
var WINDOW_MS = 12e4;
var LaunchHook = class {
  constructor(binDir, basePath = () => process.env.PATH) {
    this.binDir = binDir;
    this.basePath = basePath;
  }
  pending = [];
  expect(launch) {
    const entry = { ...launch, at: Date.now() };
    this.pending.push(entry);
    return entry;
  }
  forget(launchId) {
    this.pending = this.pending.filter((p) => p.launchId !== launchId);
  }
  /** Finds the oldest launch of ours this event belongs to. */
  match(event) {
    const now = Date.now();
    this.pending = this.pending.filter((p) => now - p.at < WINDOW_MS);
    const open = this.pending.filter((p) => !p.matched);
    const sameAgent = (p) => !event.agent || !p.agent || sameKind(p.agent, event.agent);
    return open.find((p) => event.workspaceId !== null && p.workspaceId === event.workspaceId && sameAgent(p)) ?? // Orca may not tie the pane to a workspace (null); fall back to the agent kind alone.
    (event.workspaceId === null ? open.find(sameAgent) : void 0);
  }
  /** The hook handler: returns next(event with env) for ours, nothing for others. */
  handle(event, next) {
    const ours = this.match(event);
    if (!ours) return void 0;
    ours.matched = true;
    if (event.terminalId) ours.terminalId = event.terminalId;
    const path = event.env?.PATH ?? this.basePath();
    const env = {
      ...event.env ?? {},
      ...ours.env,
      ATRIUM_NODE: process.execPath,
      PATH: path ? `${this.binDir}${delimiter}${path}` : this.binDir
    };
    return next({ ...event, env });
  }
  register(host) {
    return host.hook("agent.launch.before", (event, next) => this.handle(event, next));
  }
};
function sameKind(a, b) {
  const norm = (s) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
  return norm(a) === norm(b) || norm(a).startsWith(norm(b)) || norm(b).startsWith(norm(a));
}

// worker/src/engine/agents.ts
import { randomUUID } from "node:crypto";

// worker/src/i18n.ts
function resolveLang(setting, env = process.env) {
  if (setting === "zh" || setting === "en") return setting;
  const tag = env.LC_ALL || env.LC_MESSAGES || env.LANG || Intl.DateTimeFormat().resolvedOptions().locale || "";
  return /^zh/i.test(tag) ? "zh" : "en";
}
function stepFooter(lang, f) {
  const where = f.item ? `${f.step} / ${f.item}` : f.step;
  if (lang === "zh") {
    return [
      "",
      "---",
      `\u3010Atrium\u3011\u8FD9\u662F\u5DE5\u4F5C\u6D41\u6B65\u9AA4\u300C${where}\u300D\u3002\u8BF7\u5728\u6587\u4EF6\u5939 ${f.dir} \u4E2D\u5DE5\u4F5C\uFF08\u5148 cd \u8FDB\u53BB\uFF09\u3002`,
      "\u5B8C\u6210\u540E\uFF0C\u5FC5\u987B\u8FD0\u884C\u4E0B\u9762\u7684\u547D\u4EE4\u62A5\u544A\u7ED3\u679C\uFF08\u4E00\u6BB5\u8BDD\u7684\u603B\u7ED3\uFF1B\u8F83\u957F\u7684\u62A5\u544A\u5199\u8FDB\u6587\u4EF6\u518D\u7528 -f\uFF09\uFF1A",
      '  atrium report "<\u603B\u7ED3>"    \u6216    atrium report -f <\u6587\u4EF6>',
      '\u5982\u679C\u65E0\u6CD5\u5B8C\u6210\uFF0C\u8FD0\u884C atrium report "BLOCKED: <\u539F\u56E0>"\u3002\u9700\u8981\u95EE\u4EBA\u65F6\u8FD0\u884C atrium ask "<\u95EE\u9898>"\uFF0C\u7136\u540E\u7ED3\u675F\u672C\u8F6E\u7B49\u5F85\u56DE\u590D\u3002',
      `\u627E\u4E0D\u5230 atrium \u547D\u4EE4\u65F6\uFF0C\u7528\uFF1A${f.cli}`
    ].join("\n");
  }
  return [
    "",
    "---",
    `[Atrium] This is workflow step "${where}". Work in the folder ${f.dir} (cd there first).`,
    "When you are done you MUST report with one of these (a one-paragraph summary; put a long report in a file and use -f):",
    '  atrium report "<summary>"    or    atrium report -f <file>',
    'If you cannot finish, run atrium report "BLOCKED: <why>". To ask the person, run atrium ask "<question>" and end your turn to wait for the answer.',
    `If the atrium command is not found, use: ${f.cli}`
  ].join("\n");
}
function roleFooter(lang, f) {
  const task = f.task ?? "<id>";
  if (lang === "zh") {
    return [
      "",
      "---",
      `\u3010Atrium\u3011\u4F60\u5728\u9879\u76EE ${f.root} \u4E2D\u62C5\u4EFB ${f.role}\u3002\u7528\u8FD9\u4E9B\u547D\u4EE4\u548C Atrium \u6C9F\u901A\uFF1A`,
      `  atrium task done ${task} "<\u7ED3\u679C>"       \u4EFB\u52A1\u5B8C\u6210`,
      `  atrium task blocked ${task} "<\u539F\u56E0>"    \u4EFB\u52A1\u53D7\u963B`,
      '  atrium notice "<\u6D88\u606F>"                   \u7ED9\u9879\u76EE\u53D1\u901A\u77E5',
      '  atrium ask "<\u95EE\u9898>"                      \u9700\u8981\u4EBA\u6765\u56DE\u7B54',
      "  atrium status                            \u67E5\u770B\u4F60\u7684\u5206\u5DE5",
      `\u627E\u4E0D\u5230 atrium \u547D\u4EE4\u65F6\uFF0C\u7528\uFF1A${f.cli}`
    ].join("\n");
  }
  return [
    "",
    "---",
    `[Atrium] You are ${f.role} in the project ${f.root}. Talk to Atrium with these commands:`,
    `  atrium task done ${task} "<result>"      a task is finished`,
    `  atrium task blocked ${task} "<why>"      a task is blocked`,
    '  atrium notice "<text>"                   post a notice to the project',
    '  atrium ask "<question>"                  a question only the person can answer',
    "  atrium status                            what you are assigned",
    `If the atrium command is not found, use: ${f.cli}`
  ].join("\n");
}
function nudgeText(lang, kind) {
  if (lang === "zh") {
    return kind === "step" ? '\u63D0\u9192\uFF1A\u4F60\u8FD8\u6CA1\u6709\u62A5\u544A\u7ED3\u679C\u3002\u5982\u679C\u8FD9\u4E00\u6B65\u5DF2\u5B8C\u6210\uFF0C\u8BF7\u8FD0\u884C atrium report "<\u603B\u7ED3>"\uFF1B\u5982\u679C\u8FD8\u6CA1\u5B8C\u6210\uFF0C\u8BF7\u7EE7\u7EED\u3002' : '\u63D0\u9192\uFF1A\u5982\u679C\u5F53\u524D\u4EFB\u52A1\u5DF2\u5B8C\u6210\uFF0C\u8BF7\u8FD0\u884C atrium task done <id> "<\u7ED3\u679C>"\uFF1B\u5982\u679C\u53D7\u963B\uFF0C\u8FD0\u884C atrium task blocked <id> "<\u539F\u56E0>"\uFF1B\u5426\u5219\u8BF7\u7EE7\u7EED\u3002';
  }
  return kind === "step" ? 'Reminder: you have not reported yet. If this step is done, run atrium report "<summary>"; if not, please carry on.' : 'Reminder: if your current task is done, run atrium task done <id> "<result>"; if it is blocked, atrium task blocked <id> "<why>"; otherwise please carry on.';
}
function standInNote(lang, member, original) {
  return lang === "zh" ? `\u3010Atrium\u3011${original} \u7684\u7528\u91CF\u5DF2\u5230\u4E0A\u9650\uFF0C\u7531\u4F60\uFF08${member}\uFF09\u4F5C\u4E3A\u66FF\u8865\u6682\u65F6\u63A5\u624B\u3002\u8BF7\u63A5\u7740\u5B83\u7684\u5DE5\u4F5C\u7EE7\u7EED\uFF0C\u5148\u770B\u770B\u6587\u4EF6\u91CC\u5DF2\u6709\u7684\u8FDB\u5C55\u3002

` : `[Atrium] ${original} hit a usage limit; you (${member}) are standing in. Continue its work; first look at the progress already in the files.

`;
}
function reviewPrompt(lang, rubric, files, dir) {
  const list = files.length ? files.join(", ") : lang === "zh" ? "\uFF08\u8BE5\u6587\u4EF6\u5939\u4E2D\u7684\u4EA7\u51FA\uFF09" : "(the outputs in the folder)";
  if (lang === "zh") {
    return [
      `\u8BF7\u5BA1\u9605 ${dir} \u4E2D\u7684\u8FD9\u4E9B\u6587\u4EF6\uFF1A${list}\u3002`,
      `\u6807\u51C6\uFF1A${rubric}`,
      '\u5BA1\u9605\u5B8C\u6210\u540E\u8FD0\u884C atrium report "PASS: <\u7406\u7531>" \u6216 atrium report "FAIL: <\u95EE\u9898\u548C\u4FEE\u6539\u5EFA\u8BAE>"\u3002\u7B2C\u4E00\u4E2A\u8BCD\u5FC5\u987B\u662F PASS \u6216 FAIL\u3002'
    ].join("\n");
  }
  return [
    `Review these files in ${dir}: ${list}.`,
    `Rubric: ${rubric}`,
    'When done, run atrium report "PASS: <why>" or atrium report "FAIL: <problems and how to fix them>". The first word must be PASS or FAIL.'
  ].join("\n");
}
function answerText(lang, answer) {
  return lang === "zh" ? `\u3010Atrium\u3011\u56DE\u590D\u4F60\u7684\u95EE\u9898\uFF1A${answer}` : `[Atrium] Answer to your question: ${answer}`;
}
function statusText(lang, n, m) {
  return lang === "zh" ? `${n} \u9879\u9700\u8981\u4F60 \xB7 ${m} \u4E2A\u8FD0\u884C\u4E2D` : `${n} need you \xB7 ${m} running`;
}
var T = {
  stalled: (lang, what) => lang === "zh" ? `${what} \u505C\u4E0B\u4E86\uFF0C\u6CA1\u6709\u62A5\u544A` : `${what} stopped without reporting`,
  blocked: (lang, what) => lang === "zh" ? `${what} \u9700\u8981\u4F60\u6279\u51C6` : `${what} is waiting for approval`,
  question: (lang, what) => lang === "zh" ? `${what} \u6709\u95EE\u9898\u8981\u95EE\u4F60` : `${what} has a question`,
  gate: (lang, what) => lang === "zh" ? `${what} \u7B49\u4F60\u51B3\u5B9A` : `${what} needs your decision`,
  quota: (lang, member, standIn) => lang === "zh" ? standIn ? `${member} \u5DF2\u8FBE\u7528\u91CF\u4E0A\u9650\uFF0C\u66FF\u8865 ${standIn} \u63A5\u624B` : `${member} \u5DF2\u8FBE\u7528\u91CF\u4E0A\u9650\uFF0C\u6CA1\u6709\u53EF\u7528\u7684\u66FF\u8865` : standIn ? `${member} hit its limit; ${standIn} stands in` : `${member} hit its limit; no stand-in is free`,
  error: (lang, what) => lang === "zh" ? `${what} \u51FA\u9519\u4E86` : `${what} failed`,
  noWorkspace: (lang) => lang === "zh" ? "\u8BF7\u5148\u5728 Orca \u4E2D\u6253\u5F00\u4E00\u4E2A\u5DE5\u4F5C\u533A\u3002" : "Open a workspace in Orca first.",
  openHint: (lang) => lang === "zh" ? "Atrium \u5728\u53F3\u4FA7\u8FB9\u680F\uFF08\u56FE\u5C42\u56FE\u6807\uFF09\u3002" : "Atrium is in the right sidebar (the layers icon).",
  pickTemplate: (lang) => lang === "zh" ? "\u5728 Atrium \u9762\u677F\u7684\u300C\u5DE5\u4F5C\u6D41\u300D\u91CC\u9009\u62E9\u4E00\u4E2A\u6A21\u677F\u5F00\u59CB\u3002" : "Pick a template under Workflows in the Atrium panel."
};

// worker/src/engine/agents.ts
var QUOTA = [
  /usage limit/i,
  /rate[- ]limit(ed)?\b.*(reached|exceeded|hit)/i,
  /(hit|reached) (your|the) (usage |daily |weekly )?limit/i,
  /quota (exceeded|exhausted|reached)/i,
  /out of (credits|tokens)/i,
  /limit (will )?resets? (at|in)/i,
  /用量(已)?(达到)?上限/,
  /额度(已)?(用完|耗尽|不足)/
];
function quotaEvidence(lines) {
  const tail = lines.slice(-40);
  for (const line of tail.reverse()) {
    if (QUOTA.some((re) => re.test(line))) return line.trim().slice(0, 300);
  }
  return null;
}
var Agents = class {
  constructor(host, store, hook, opts, lang) {
    this.host = host;
    this.store = store;
    this.hook = hook;
    this.opts = opts;
    this.lang = lang;
  }
  liveMap = /* @__PURE__ */ new Map();
  timers = /* @__PURE__ */ new Map();
  launching = Promise.resolve();
  events = {};
  list() {
    return [...this.liveMap.values()];
  }
  stateOf(terminalId) {
    return terminalId ? this.liveMap.get(terminalId)?.state : void 0;
  }
  /** Bound agents still alive. */
  running() {
    return this.store.live().length;
  }
  /** Starts an agent with our env. One launch at a time, so the hook matches the right one. */
  launch(req) {
    const run = this.launching.then(() => this.launchNow(req));
    this.launching = run.catch(() => {
    });
    return run;
  }
  async launchNow(req) {
    const launchId = randomUUID();
    const env = {
      ATRIUM_SOCKET: this.opts.socket,
      ATRIUM_TOKEN: this.opts.token,
      ATRIUM_LAUNCH: launchId,
      ...req.env
    };
    const pending = this.hook.expect({ launchId, workspaceId: req.workspaceId, agent: req.agent, env });
    let terminalId;
    try {
      const answer = await this.host.call("agent.launch", {
        workspaceId: req.workspaceId,
        agent: req.agent,
        prompt: req.prompt
      });
      terminalId = answer.terminalId;
    } finally {
      this.hook.forget(launchId);
    }
    if (!pending.matched) this.host.log(`agents: launch ${launchId} in ${req.workspaceId} did not pass our hook`);
    const binding = {
      ...req.binding,
      launchId,
      terminalId,
      workspaceId: req.workspaceId,
      agent: req.agent,
      startedAt: Date.now(),
      reported: false,
      nudged: false
    };
    this.store.addBinding(binding);
    this.setLive(terminalId, { workspaceId: req.workspaceId, agent: req.agent, state: "starting", binding });
    return binding;
  }
  setLive(terminalId, patch) {
    const old = this.liveMap.get(terminalId);
    const binding = patch.binding ?? this.store.byTerminal(terminalId);
    const live = {
      terminalId,
      workspaceId: patch.workspaceId ?? old?.workspaceId ?? binding?.workspaceId ?? null,
      agent: patch.agent ?? old?.agent ?? binding?.agent ?? "agent",
      ...patch.title ?? old?.title ? { title: patch.title ?? old?.title } : {},
      state: patch.state,
      ...binding ? { bound: boundOf(binding) } : {},
      since: old && old.state === patch.state ? old.since : Date.now()
    };
    this.liveMap.set(terminalId, live);
    if (!old || old.state !== patch.state) {
      this.host.post("agent.changed", { terminalId, state: patch.state });
      this.events.changed?.(binding, patch.state, terminalId);
    }
  }
  /** `agent.status.changed`. */
  async onStatus(payload) {
    const terminalId = payload.terminalId;
    if (!terminalId) return;
    const binding = this.store.byTerminal(terminalId);
    this.setLive(terminalId, {
      workspaceId: payload.workspaceId ?? null,
      ...payload.title ? { title: payload.title } : {},
      state: payload.state
    });
    if (!binding) return;
    this.clearTimer(terminalId);
    if (payload.state === "working") return;
    if (await this.checkQuota(binding)) return;
    if (payload.state === "blocked") {
      this.events.blocked?.(binding);
      return;
    }
    this.events.idle?.(binding);
    if (binding.kind !== "role" && !binding.reported) this.armNudge(binding);
  }
  armNudge(binding) {
    const terminalId = binding.terminalId;
    if (!terminalId) return;
    this.timers.set(
      terminalId,
      setTimeout(() => void this.afterGrace(binding), this.opts.graceMs)
    );
  }
  async afterGrace(binding) {
    if (!binding.terminalId) return;
    this.timers.delete(binding.terminalId);
    if (binding.reported || binding.ended) return;
    const state = this.stateOf(binding.terminalId);
    if (state === "working" || state === "starting") return;
    if (!binding.nudged) {
      binding.nudged = true;
      this.store.save();
      await this.nudge(binding.terminalId, binding.kind === "role" ? "role" : "step");
      this.armNudge(binding);
      return;
    }
    this.setLive(binding.terminalId, { state: "stalled" });
    this.events.stalled?.(binding);
  }
  async nudge(terminalId, kind) {
    const binding = this.store.byTerminal(terminalId);
    const text2 = nudgeText(this.lang(), kind ?? (binding?.kind === "role" ? "role" : "step"));
    await this.send(terminalId, text2);
  }
  async send(terminalId, text2) {
    try {
      const answer = await this.host.call("terminal.sendText", { terminalId, text: text2, enter: true });
      return answer?.accepted !== false;
    } catch (error) {
      this.host.log(`agents: send to ${terminalId}: ${String(error)}`);
      return false;
    }
  }
  async checkQuota(binding) {
    if (!binding.terminalId || !this.host.has("terminal.read")) return false;
    let lines = [];
    try {
      lines = (await this.host.call("terminal.read", { terminalId: binding.terminalId, maxLines: 80 })).lines ?? [];
    } catch {
      return false;
    }
    const evidence = quotaEvidence(lines);
    if (!evidence) return false;
    if (binding.member) {
      this.store.data.quotas[binding.member] = { ...this.store.data.quotas[binding.member] ?? {}, limited: true };
      this.store.save();
    }
    this.events.quota?.(binding, evidence);
    return true;
  }
  /** The agent reported: no nudge, no stall. */
  reported(binding) {
    binding.reported = true;
    this.store.save();
    if (binding.terminalId) {
      this.clearTimer(binding.terminalId);
      if (this.stateOf(binding.terminalId) === "stalled") this.setLive(binding.terminalId, { state: "done" });
    }
  }
  /** Stops driving this binding; closes the terminal when asked. */
  async release(binding, close) {
    this.store.end(binding);
    if (!binding.terminalId) return;
    this.clearTimer(binding.terminalId);
    if (close) {
      try {
        await this.host.call("terminal.close", { terminalId: binding.terminalId });
      } catch (error) {
        this.host.log(`agents: close ${binding.terminalId}: ${String(error)}`);
      }
      this.liveMap.delete(binding.terminalId);
      this.host.post("agent.changed", { terminalId: binding.terminalId, state: "idle" });
    } else {
      const live = this.liveMap.get(binding.terminalId);
      if (live) {
        delete live.bound;
      }
    }
  }
  async focus(terminalId) {
    await this.host.call("terminal.focus", { terminalId });
  }
  /** After a restart: keep bindings whose terminals Orca still lists, drop the rest. */
  async reconcile() {
    const byWorkspace = /* @__PURE__ */ new Map();
    for (const b of this.store.live()) {
      const list = byWorkspace.get(b.workspaceId) ?? [];
      list.push(b);
      byWorkspace.set(b.workspaceId, list);
    }
    for (const [workspaceId, bindings] of byWorkspace) {
      let agents;
      try {
        agents = (await this.host.call("agents.list", { workspaceId })).agents ?? [];
      } catch (error) {
        const code = error.code;
        if (code !== "invalid_params" && code !== "action_failed") continue;
        agents = [];
      }
      for (const b of bindings) {
        const found = agents.find((a) => a.terminalId === b.terminalId);
        if (found && b.terminalId) {
          this.setLive(b.terminalId, { workspaceId, agent: found.agent ?? b.agent, ...found.title ? { title: found.title } : {}, state: found.state, binding: b });
          if (found.state !== "working" && b.kind !== "role" && !b.reported) this.armNudge(b);
        } else {
          this.store.end(b);
          this.events.gone?.(b);
        }
      }
    }
  }
  clearTimer(terminalId) {
    const timer = this.timers.get(terminalId);
    if (timer) clearTimeout(timer);
    this.timers.delete(terminalId);
  }
  dispose() {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
  }
};
function boundOf(b) {
  if (b.kind === "role") return { kind: "role", root: b.root ?? "", role: b.role ?? "", ...b.task ? { task: b.task } : {} };
  return { kind: "step", dir: b.dir ?? "", step: b.step ?? "", ...b.item ? { item: b.item } : {} };
}

// worker/src/engine/cli.ts
var HELP_EN = [
  "atrium report <text> | -f <file>       report the result of your workflow step",
  "atrium status                          what you are assigned",
  "atrium task done|blocked <id> [note]   mark a project task",
  "atrium notice <text>                   post a notice to the project",
  'atrium ask <question>                  ask the person (a "needs you" item)'
].join("\n");
var HELP_ZH = [
  "atrium report <\u6587\u672C> | -f <\u6587\u4EF6>        \u62A5\u544A\u5DE5\u4F5C\u6D41\u6B65\u9AA4\u7684\u7ED3\u679C",
  "atrium status                          \u67E5\u770B\u5206\u7ED9\u4F60\u7684\u5DE5\u4F5C",
  "atrium task done|blocked <id> [\u8BF4\u660E]    \u6807\u8BB0\u9879\u76EE\u4EFB\u52A1",
  "atrium notice <\u6587\u672C>                    \u7ED9\u9879\u76EE\u53D1\u901A\u77E5",
  "atrium ask <\u95EE\u9898>                       \u5411\u7528\u6237\u63D0\u95EE\uFF08\u8FDB\u5165\u300C\u9700\u8981\u4F60\u300D\uFF09"
].join("\n");
function makeCliHandler(ctx, runs, projects) {
  const bindingOf = (r) => {
    const c = r.context ?? {};
    return (r.launch ? ctx.store.byLaunch(r.launch) : void 0) ?? (r.terminal ? ctx.store.byTerminal(r.terminal) : void 0) ?? (c.ATRIUM_RUN && c.ATRIUM_STEP ? ctx.store.forStep(c.ATRIUM_RUN, c.ATRIUM_STEP, c.ATRIUM_ITEM) : void 0) ?? (c.ATRIUM_PROJECT && c.ATRIUM_ROLE ? ctx.store.forRole(c.ATRIUM_PROJECT, c.ATRIUM_ROLE) : void 0);
  };
  const fail = (error) => ({ ok: false, error });
  return async (request) => {
    const zh = ctx.lang() === "zh";
    if (request.cmd === "help") return { ok: true, text: zh ? HELP_ZH : HELP_EN };
    const b = bindingOf(request);
    if (!b) return fail(zh ? "\u8FD9\u4E2A\u7EC8\u7AEF\u4E0D\u662F\u7531 Atrium \u542F\u52A8\u7684\u3002" : "This terminal was not started by Atrium.");
    const text2 = request.text ?? request.args.join(" ");
    const role = b.kind === "role";
    switch (request.cmd) {
      case "report":
        if (!text2.trim()) return fail(zh ? "\u62A5\u544A\u5185\u5BB9\u4E3A\u7A7A\u3002" : "The report is empty.");
        if (role) {
          projects.notice(b.root, text2, b.role);
          return { ok: true, text: zh ? "\u5DF2\u8BB0\u5165\u9879\u76EE\u901A\u77E5\u3002" : "Added to the project notices." };
        }
        return { ok: true, text: await runs.report(b, text2) };
      case "status":
        if (role) return { ok: true, text: projects.statusText(b) };
        return { ok: true, text: `run ${b.dir}
step ${b.step}${b.item ? ` / ${b.item}` : ""}${b.kind === "review" ? " (review)" : ""}` };
      case "task": {
        const [outcome, id, ...note] = request.args;
        if (!role) return fail(zh ? "task \u53EA\u7528\u4E8E\u9879\u76EE\u89D2\u8272\u3002" : "`task` is for project roles.");
        if (outcome !== "done" && outcome !== "blocked" || !id) return fail("usage: atrium task done|blocked <id> [note]");
        return { ok: true, text: projects.taskReport(b, id, outcome, request.text ?? note.join(" ")) };
      }
      case "notice":
        if (!role) return fail(zh ? "notice \u53EA\u7528\u4E8E\u9879\u76EE\u89D2\u8272\u3002" : "`notice` is for project roles.");
        if (!text2.trim()) return fail(zh ? "\u901A\u77E5\u5185\u5BB9\u4E3A\u7A7A\u3002" : "The notice is empty.");
        projects.notice(b.root, text2, b.role);
        return { ok: true, text: zh ? "\u5DF2\u53D1\u5E03\u3002" : "Posted." };
      case "ask":
        if (!text2.trim()) return fail(zh ? "\u95EE\u9898\u4E3A\u7A7A\u3002" : "The question is empty.");
        return { ok: true, text: projects.ask(b, text2) };
      default:
        return fail(`unknown command ${request.cmd}
${zh ? HELP_ZH : HELP_EN}`);
    }
  };
}

// worker/src/engine/context.ts
function errorText(error) {
  return error instanceof Error ? error.message : String(error);
}

// worker/src/engine/needs.ts
var Needs = class {
  constructor(host, store, lang) {
    this.host = host;
    this.store = store;
    this.lang = lang;
  }
  resolvers = /* @__PURE__ */ new Map();
  running = () => 0;
  list() {
    return [...this.store.data.needs].sort((a, b) => a.createdAt - b.createdAt);
  }
  get(id) {
    return this.store.data.needs.find((n) => n.id === id);
  }
  /** Per kind: what resolving a need does (send an answer, decide a gate…). */
  onResolve(kind, resolver) {
    this.resolvers.set(kind, resolver);
  }
  countRunning(fn) {
    this.running = fn;
  }
  /** Adds or replaces by id. Notifies only for a new item. */
  add(need, notify = true) {
    const list = this.store.data.needs;
    const at = list.findIndex((n) => n.id === need.id);
    if (at >= 0) {
      list[at] = { ...need, createdAt: list[at].createdAt };
    } else {
      list.push(need);
      if (notify) void this.notify(need);
    }
    this.store.save();
    this.changed();
  }
  remove(id) {
    const before = this.store.data.needs.length;
    this.store.data.needs = this.store.data.needs.filter((n) => n.id !== id);
    if (this.store.data.needs.length === before) return false;
    this.store.save();
    this.changed();
    return true;
  }
  removeWhere(pred) {
    const before = this.store.data.needs.length;
    this.store.data.needs = this.store.data.needs.filter((n) => !pred(n));
    if (this.store.data.needs.length !== before) {
      this.store.save();
      this.changed();
    }
  }
  async resolve(id, answer) {
    const need = this.get(id);
    if (!need) return;
    const resolver = this.resolvers.get(need.kind);
    if (resolver) await resolver(need, answer);
    this.remove(id);
  }
  forRoot(root) {
    return this.store.data.needs.filter((n) => n.source.kind === "project" && n.source.root === root).length;
  }
  /** Status bar item and posts. */
  changed() {
    const count = this.store.data.needs.length;
    this.host.post("needs.changed", { count });
    this.host.post("overview.changed");
    void this.updateStatusBar();
  }
  async updateStatusBar() {
    const count = this.store.data.needs.length;
    const running = this.running();
    try {
      await this.host.call("statusBar.update", {
        id: "needs",
        text: statusText(this.lang(), count, running),
        tone: count > 0 ? "warning" : running > 0 ? "accent" : "neutral",
        visible: true
      });
    } catch {
    }
  }
  async notify(need) {
    try {
      await this.host.call("notifications.show", { title: need.title, ...need.detail ? { body: need.detail.slice(0, 300) } : {} });
    } catch {
    }
  }
};

// worker/src/engine/projects.ts
import { existsSync as existsSync3 } from "node:fs";
import { basename, join as join3 } from "node:path";

// worker/src/coreapi.ts
var str = (value) => typeof value === "string" && value ? value : void 0;
function kindOf(raw) {
  return String(raw.kind ?? raw.type ?? raw.effect ?? "");
}
function labelled(list, key2) {
  if (!Array.isArray(list)) return [];
  return list.map((entry) => {
    if (typeof entry === "string") return { id: entry, label: entry };
    const obj2 = entry;
    const id = String(obj2[key2] ?? obj2.id ?? obj2.step ?? "");
    return { id, label: String(obj2.label ?? obj2.title ?? id) };
  });
}
function normalizeEffects(raw) {
  const list = Array.isArray(raw) ? raw : raw && typeof raw === "object" && Array.isArray(raw.effects) ? raw.effects : raw && typeof raw === "object" ? [raw] : [];
  const out = [];
  for (const entry of list) {
    if (!entry || typeof entry !== "object") continue;
    const e = entry;
    const step = String(e.step ?? "");
    const item = str(e.item);
    switch (kindOf(e)) {
      case "launch":
        out.push({
          kind: "launch",
          step,
          ...item ? { item } : {},
          prompt: String(e.prompt ?? ""),
          ...str(e.agent) ? { agent: str(e.agent) } : {},
          ...e.worktree === true ? { worktree: true } : {}
        });
        break;
      case "check": {
        const nested = e.check && typeof e.check === "object" ? e.check : null;
        const check = nested ? { ...nested, kind: String(nested.kind ?? nested.type ?? "") } : { ...e, kind: String(e.check_kind ?? e.checkKind ?? "") };
        out.push({
          kind: "check",
          step,
          ...item ? { item } : {},
          ...typeof e.index === "number" ? { index: e.index } : {},
          check
        });
        break;
      }
      case "gate": {
        const sendBack = labelled(e.send_back ?? e.sendBack ?? e.fixes, "step").map((x) => ({ step: x.id, label: x.label }));
        out.push({
          kind: "gate",
          step,
          ...str(e.title) ? { title: str(e.title) } : {},
          ...str(e.detail ?? e.prompt) ? { detail: str(e.detail ?? e.prompt) } : {},
          choices: labelled(e.choices, "id"),
          sendBack
        });
        break;
      }
      case "done":
        out.push({ kind: "done", ...str(e.state) ? { state: str(e.state) } : {} });
        break;
      default:
        out.push({ kind: "wait" });
    }
  }
  return out;
}
function normalizeActions(raw) {
  const list = Array.isArray(raw) ? raw : raw && typeof raw === "object" ? raw.actions ?? raw.effects ?? [] : [];
  const out = [];
  if (!Array.isArray(list)) return out;
  for (const entry of list) {
    if (!entry || typeof entry !== "object") continue;
    const e = entry;
    const role = String(e.role ?? e.stream ?? "");
    switch (kindOf(e)) {
      case "launch":
      case "start":
        out.push({
          kind: "launch",
          role,
          ...str(e.member) ? { member: str(e.member) } : {},
          ...str(e.agent) ? { agent: str(e.agent) } : {},
          ...str(e.model) ? { model: str(e.model) } : {},
          ...str(e.prompt) ? { prompt: str(e.prompt) } : {},
          ...str(e.task) ? { task: str(e.task) } : {},
          ...e.worktree === true ? { worktree: true } : {},
          ...str(e.cwd) ? { cwd: str(e.cwd) } : {},
          ...str(e.standingInFor) ? { standingInFor: str(e.standingInFor) } : {}
        });
        break;
      case "send":
      case "wake":
      case "message":
        out.push({ kind: "send", role, text: String(e.text ?? e.prompt ?? "") });
        break;
      case "stop":
      case "retire":
        out.push({ kind: "stop", role });
        break;
      case "need":
      case "ask":
        out.push({
          kind: "need",
          title: String(e.title ?? e.text ?? ""),
          ...str(e.detail) ? { detail: str(e.detail) } : {},
          ...role ? { role } : {},
          ...str(e.needKind ?? e.need) ? { needKind: str(e.needKind ?? e.need) } : {},
          ...str(e.task) ? { task: str(e.task) } : {}
        });
        break;
    }
  }
  return out;
}
function normalizePick(raw) {
  if (!raw || typeof raw !== "object") return null;
  const r = raw;
  const inner = r.pick && typeof r.pick === "object" ? r.pick : r;
  const member = str(inner.member ?? inner.name);
  if (!member) return null;
  const agent = str(inner.agent);
  const model = str(inner.model);
  const instead = inner.instead_of ?? inner.insteadOf;
  const insteadName = Array.isArray(instead) ? instead[0] : instead && typeof instead === "object" ? instead.member : instead;
  const standingInFor = str(inner.standing_in_for ?? inner.standingInFor ?? insteadName);
  return { member, ...agent ? { agent } : {}, ...model ? { model } : {}, ...standingInFor ? { standingInFor } : {} };
}

// worker/src/workspaces.ts
import { isAbsolute, relative, resolve } from "node:path";
function contains(parent, child) {
  const rel = relative(resolve(parent), resolve(child));
  return rel === "" || !rel.startsWith("..") && !isAbsolute(rel);
}
var Workspaces = class {
  constructor(host) {
    this.host = host;
  }
  rows = [];
  fetchedAt = 0;
  focused = null;
  async list(fresh = false) {
    if (fresh || Date.now() - this.fetchedAt > 5e3 || this.rows.length === 0) {
      try {
        const answer = await this.host.call("workspace.list", {});
        this.rows = answer.workspaces ?? [];
        this.fetchedAt = Date.now();
      } catch (error) {
        this.host.log(`workspaces: list failed: ${String(error)}`);
      }
    }
    return this.rows;
  }
  /** A workspace from the last listing, without asking Orca. */
  cached(id) {
    return this.rows.find((w) => w.id === id);
  }
  invalidate() {
    this.fetchedAt = 0;
  }
  async byId(id) {
    return (await this.list()).find((w) => w.id === id) ?? (await this.list(true)).find((w) => w.id === id);
  }
  /** The local workspace whose folder holds `path` most closely. */
  async forPath(path) {
    const pick = (rows) => rows.filter((w) => (w.hostId ?? "local") === "local" && contains(w.path, path)).sort((a, b) => b.path.length - a.path.length)[0];
    return pick(await this.list()) ?? pick(await this.list(true));
  }
  async focusedWorkspace() {
    let id = this.focused?.workspaceId;
    if (!id) {
      try {
        const ctx = await this.host.call("workspace.readContext", {});
        this.focused = ctx;
        id = ctx?.workspaceId;
      } catch {
      }
    }
    return id ? this.byId(id) : void 0;
  }
  /** A new worktree next to `base`, for a step or role that works on its own branch. */
  async createWorktree(base, name) {
    if (!base.projectId) throw new Error(`workspace ${base.name} is not in a Git project`);
    const answer = await this.host.call("workspace.create", {
      projectId: base.projectId,
      ...base.hostId && base.hostId !== "local" ? { hostId: base.hostId } : {},
      name: safeName(name),
      ...base.branch ? { baseBranch: base.branch } : {}
    });
    this.invalidate();
    return answer.workspaceId;
  }
  /** The path of `abs` relative to the workspace, as editor.open takes it. */
  relative(workspace, abs) {
    return relative(workspace.path, abs).split("\\").join("/");
  }
};
function safeName(name) {
  return name.toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60) || "atrium";
}

// worker/src/engine/projects.ts
var STATUSES = ["todo", "ready", "in_progress", "review", "blocked", "done"];
var LIMIT_MS = 5 * 36e5;
var HEARTBEAT_MS = 10 * 6e4;
function launchSpec(raw, defaultAgent) {
  return {
    role: String(raw.role ?? ""),
    agent: text(raw.agent) ?? defaultAgent,
    prompt: String(raw.prompt ?? ""),
    ...text(raw.cwd) ? { cwd: raw.cwd } : {},
    worktree: raw.worktree === true,
    ...text(raw.task) ? { task: raw.task } : {},
    ...text(raw.member) ? { member: raw.member } : {},
    ...text(raw.model) ? { model: raw.model } : {},
    ...text(raw.standingInFor) ? { standingInFor: raw.standingInFor } : {}
  };
}
var obj = (v) => v && typeof v === "object" ? v : {};
var text = (v) => typeof v === "string" && v ? v : void 0;
function mapTask(raw) {
  const r = obj(raw);
  const status = STATUSES.includes(r.status) ? r.status : r.status === "in-progress" || r.status === "doing" ? "in_progress" : "todo";
  const task = { id: String(r.id ?? ""), title: String(r.title ?? r.id ?? ""), status };
  if (text(r.owner)) task.owner = r.owner;
  if (text(r.milestone)) task.milestone = r.milestone;
  if (text(r.phase)) task.phase = r.phase;
  if (text(r.file ?? r.path)) task.file = r.file ?? r.path;
  if (r.pool === true) task.pool = true;
  const at = r.updatedAt ?? r.updated_at;
  if (typeof at === "number") task.updatedAt = at;
  return task;
}
function mapMember(raw) {
  const r = obj(raw);
  const m = { name: String(r.name ?? ""), agent: String(r.agent ?? "") };
  if (text(r.model)) m.model = r.model;
  if (text(r.effort)) m.effort = r.effort;
  if (Array.isArray(r.folders) && r.folders.length) m.folders = r.folders.map(String);
  if (text(r.notes)) m.notes = r.notes;
  const standIns = r.standIns ?? r.stand_ins;
  if (Array.isArray(standIns) && standIns.length) m.standIns = standIns.map(String);
  return m;
}
function mapErrors(raw) {
  if (!Array.isArray(raw)) return typeof raw === "string" && raw ? [{ message: raw }] : [];
  return raw.map((e) => typeof e === "string" ? { message: e } : { ...typeof obj(e).line === "number" ? { line: obj(e).line } : {}, message: String(obj(e).message ?? e) });
}
function flatten(node, out = []) {
  out.push(node);
  for (const c of node.children) flatten(c, out);
  return out;
}
var Projects = class {
  constructor(ctx) {
    this.ctx = ctx;
  }
  chains = /* @__PURE__ */ new Map();
  inflight = /* @__PURE__ */ new Set();
  failed = /* @__PURE__ */ new Set();
  /** Autopilot wakes that are due, by project; `project.next` consumes them. */
  wakes = /* @__PURE__ */ new Map();
  lastWake = /* @__PURE__ */ new Map();
  // ---------- known projects ----------
  metas() {
    return this.ctx.store.data.projects;
  }
  meta(root) {
    return this.metas().find((p) => p.root === root);
  }
  register(root, workspaceId) {
    let meta = this.meta(root);
    if (!meta) {
      meta = { root };
      this.metas().push(meta);
    }
    if (workspaceId) meta.workspaceId = workspaceId;
    this.ctx.store.save();
    return meta;
  }
  /** Workspaces whose folder holds a project become known projects. */
  async discover() {
    for (const ws of await this.ctx.workspaces.list()) {
      if ((ws.hostId ?? "local") !== "local") continue;
      if (existsSync3(join3(ws.path, ".atrium", "project.json"))) {
        const meta = this.meta(ws.path);
        if (!meta) this.register(ws.path, ws.id);
        else if (!meta.workspaceId) {
          meta.workspaceId = ws.id;
          this.ctx.store.save();
        }
      }
    }
    const before = this.metas().length;
    this.ctx.store.data.projects = this.metas().filter((p) => existsSync3(join3(p.root, ".atrium", "project.json")));
    if (this.metas().length !== before) this.ctx.store.save();
  }
  requireProject(root) {
    const meta = this.meta(root);
    if (!meta) {
      if (existsSync3(join3(root, ".atrium", "project.json"))) return this.register(root);
      throw new HostError("invalid_params", `${root} is not an Atrium project`);
    }
    return meta;
  }
  // ---------- reading ----------
  raw(root) {
    return obj(this.ctx.core.call("project.open", { root }));
  }
  view(root) {
    this.requireProject(root);
    const raw = this.raw(root);
    const tasks = (Array.isArray(raw.tasks) ? raw.tasks : []).map(mapTask);
    const done = tasks.filter((t) => t.status === "done").length;
    const org = this.decorate(raw.org && typeof raw.org === "object" ? raw.org : { id: "controller", role: { kind: "controller" }, children: [] }, root, tasks);
    const roles = flatten(org).map((n) => ({ id: n.id, ...n.live ? { live: n.live } : {} }));
    const view = {
      root,
      name: String(raw.name ?? basename(root)),
      autopilot: raw.autopilot === true || obj(raw.autopilot).on === true,
      progress: raw.progress && typeof raw.progress === "object" ? { done: Number(raw.progress.done ?? done), total: Number(raw.progress.total ?? tasks.length) } : { done, total: tasks.length },
      roles,
      needs: this.ctx.needs.forRoot(root),
      brief: String(raw.brief ?? ""),
      plan: String(raw.plan ?? ""),
      milestones: Array.isArray(raw.milestones) ? raw.milestones : [],
      tasks,
      org,
      team: (Array.isArray(raw.team) ? raw.team : Array.isArray(obj(raw.team).members) ? obj(raw.team).members : []).map(mapMember),
      notices: (Array.isArray(raw.notices) ? raw.notices : []).map((n) => {
        const r = obj(n);
        return { at: Number(r.at ?? 0), ...text(r.from) ? { from: r.from } : {}, text: String(r.text ?? "") };
      })
    };
    return view;
  }
  summary(root) {
    try {
      const { brief: _b, plan: _p, milestones: _m, tasks: _t, org: _o, team: _tm, notices: _n, ...summary } = this.view(root);
      return summary;
    } catch (error) {
      this.ctx.host.log(`projects: cannot read ${root}: ${errorText(error)}`);
      return null;
    }
  }
  /** Live state, terminal, current task and quota on every node. */
  decorate(node, root, tasks) {
    const binding = this.ctx.store.forRole(root, node.id);
    const member = binding?.member ?? node.member;
    const live = binding ? this.ctx.agents.stateOf(binding.terminalId) ?? "idle" : void 0;
    const quota = member ? this.ctx.store.data.quotas[member] : void 0;
    const current = node.currentTask ?? binding?.task ?? tasks.find((t) => t.owner === node.id && t.status === "in_progress")?.title;
    const out = { ...node, children: (node.children ?? []).map((c) => this.decorate(c, root, tasks)) };
    if (member) out.member = member;
    if (binding?.agent) out.agent = binding.agent;
    if (live) out.live = live;
    if (binding?.terminalId) out.terminalId = binding.terminalId;
    if (current) out.currentTask = current;
    if (quota) out.quota = limitedNow(quota, this.ctx.now()) ? quota : { ...quota, limited: false };
    if (binding?.standingInFor) out.standingInFor = binding.standingInFor;
    return out;
  }
  // ---------- requests ----------
  async init(params) {
    const ws = params.root ? await this.ctx.workspaces.forPath(params.root) : await this.ctx.workspaces.focusedWorkspace();
    const root = params.root ?? ws?.path;
    if (!root) throw new HostError("invalid_params", T.noWorkspace(this.ctx.lang()));
    this.ctx.core.call("project.init", { root, ...params.brief ? { brief: params.brief } : {} });
    this.register(root, ws?.id);
    if (this.ctx.settings().autopilotDefault) this.ctx.core.call("project.autopilot.set", { root, on: true });
    this.changed(root);
    return { root };
  }
  setAutopilot(root, on) {
    this.requireProject(root);
    this.ctx.core.call("project.autopilot.set", { root, on });
    this.changed(root);
    if (on) void this.pump(root);
  }
  taskCreate(p) {
    this.requireProject(p.root);
    const { root, ...rest } = p;
    const task = mapTask(this.ctx.core.call("project.task.create", { root, ...rest }));
    this.changed(root);
    void this.pump(root);
    return task;
  }
  taskUpdate(p) {
    this.requireProject(p.root);
    const task = mapTask(this.ctx.core.call("project.task.update", { root: p.root, id: p.id, patch: p.patch }));
    this.changed(p.root);
    return task;
  }
  taskMove(p) {
    this.requireProject(p.root);
    const task = mapTask(this.ctx.core.call("project.task.move", { root: p.root, id: p.id, status: p.status }));
    this.changed(p.root);
    void this.pump(p.root);
    return task;
  }
  /** A notice; from an agent it carries the role's name (the core's notice op is the person's). */
  notice(root, textValue, from) {
    this.requireProject(root);
    this.ctx.core.call("project.notice.add", { root, text: from ? `[${from}] ${textValue}` : textValue });
    this.changed(root);
  }
  teamGet(root) {
    this.requireProject(root);
    const raw = obj(this.ctx.core.call("team.read", { root }));
    return { members: (Array.isArray(raw.members) ? raw.members : []).map(mapMember), errors: mapErrors(raw.errors) };
  }
  teamSave(root, members) {
    this.requireProject(root);
    const raw = obj(this.ctx.core.call("team.write", { root, members }));
    this.changed(root);
    return { errors: mapErrors(raw.errors) };
  }
  changed(root) {
    this.ctx.host.post("project.changed", { root });
    this.ctx.host.post("overview.changed");
  }
  // ---------- roles ----------
  /** What the core says to launch for a role (`project.role.start`), spent members skipped. */
  plan(root, role, agent) {
    const raw = obj(this.ctx.core.call("project.role.start", { root, role, ...agent ? { agent } : {}, quotas: this.spent(this.ctx.now()), now: Math.floor(this.ctx.now() / 1e3) }));
    const l = obj(raw.launch ?? raw);
    return launchSpec({ ...l, role }, this.ctx.settings().defaultAgent);
  }
  /** Starts a role's agent (the person pressed Start, or autopilot asked). */
  async startRole(root, role) {
    this.requireProject(root);
    const existing = this.ctx.store.forRole(root, role);
    if (existing) return existing;
    return this.launchSpec(root, this.plan(root, role));
  }
  /** Launches an agent as the core described it, then tells the core which terminal runs the role. */
  async launchSpec(root, spec) {
    const meta = this.requireProject(root);
    let workspaceId = meta.workspaceId;
    if (!workspaceId) workspaceId = (await this.ctx.workspaces.forPath(root))?.id;
    if (!workspaceId) throw new HostError("action_failed", `no Orca workspace holds ${root}`);
    let where = "";
    if (spec.worktree) {
      const base = await this.ctx.workspaces.byId(workspaceId);
      if (base) {
        workspaceId = await this.ctx.workspaces.createWorktree(base, `${basename(root)}-${spec.role.replace(/[:/]/g, "-")}`);
        where = "";
      }
    } else if (spec.cwd && spec.cwd !== root) {
      where = this.ctx.lang() === "zh" ? `\u3010Atrium\u3011\u8BF7\u5728\u6587\u4EF6\u5939 ${spec.cwd} \u4E2D\u5DE5\u4F5C\uFF08\u5148 cd \u8FDB\u53BB\uFF09\u3002
` : `[Atrium] Work in the folder ${spec.cwd} (cd there first).
`;
    }
    const note = spec.standingInFor ? standInNote(this.ctx.lang(), spec.member ?? spec.agent, spec.standingInFor) : "";
    const footer = roleFooter(this.ctx.lang(), { root, role: spec.role, ...spec.task ? { task: spec.task } : {}, cli: this.ctx.cli });
    const binding = await this.ctx.agents.launch({
      workspaceId,
      agent: spec.agent,
      prompt: `${note}${where}${spec.prompt}
${footer}`,
      binding: {
        kind: "role",
        root,
        role: spec.role,
        ...spec.task ? { task: spec.task } : {},
        ...spec.member ? { member: spec.member } : {},
        ...spec.standingInFor ? { standingInFor: spec.standingInFor } : {}
      },
      env: { ATRIUM_PROJECT: root, ATRIUM_ROLE: spec.role, ...spec.task ? { ATRIUM_TASK: spec.task } : {} }
    });
    try {
      if (binding.terminalId) this.ctx.core.call("project.bind", { root, role: spec.role, thread: binding.terminalId });
    } catch (error) {
      this.ctx.host.log(`projects: bind ${spec.role}: ${errorText(error)}`);
    }
    return binding;
  }
  async stopRole(root, role) {
    const b = this.ctx.store.forRole(root, role);
    if (b) await this.ctx.agents.release(b, true);
    this.changed(root);
  }
  /** A role's turn ended: the scheduler may wake it on the next pass. */
  wake(binding, why) {
    if (!binding.root || !binding.role) return;
    const list = this.wakes.get(binding.root) ?? [];
    if (!list.some((w) => w.role === binding.role && w.why === why)) list.push({ role: binding.role, why });
    this.wakes.set(binding.root, list);
    this.lastWake.set(`${binding.root}\0${binding.role}`, this.ctx.now());
    void this.pump(binding.root);
  }
  // ---------- the autopilot ----------
  pump(root) {
    const prev = this.chains.get(root) ?? Promise.resolve();
    const next = prev.then(() => this.pass(root)).catch((error) => this.ctx.host.log(`projects: ${root}: ${errorText(error)}`));
    this.chains.set(root, next);
    return next;
  }
  async tick() {
    await Promise.all(this.metas().map((m) => this.pump(m.root)));
  }
  async settle() {
    for (let i = 0; i < 50; i++) {
      await Promise.all([...this.chains.values()]);
      await new Promise((r) => setTimeout(r, 5));
      if (this.inflight.size === 0) {
        await Promise.all([...this.chains.values()]);
        return;
      }
    }
  }
  /** What `project.next` is told: who runs, who is mid-turn, wakes that are due, who is spent. */
  stateFor(root) {
    const now = this.ctx.now();
    const live = this.ctx.store.live().filter((b) => b.kind === "role" && b.root === root);
    const mid = (b) => ["working", "starting"].includes(this.ctx.agents.stateOf(b.terminalId) ?? "idle");
    const wakes = this.wakes.get(root) ?? [];
    this.wakes.delete(root);
    for (const b of live) {
      const at = this.lastWake.get(`${root}\0${b.role}`) ?? b.startedAt;
      if (!mid(b) && now - at > HEARTBEAT_MS && !wakes.some((w) => w.role === b.role)) {
        wakes.push({ role: b.role, why: "heartbeat" });
        this.lastWake.set(`${root}\0${b.role}`, now);
      }
    }
    return {
      live: live.map((b) => b.role),
      busy: live.filter(mid).map((b) => b.role),
      wakes,
      maxRunning: Math.max(1, this.ctx.settings().maxParallel),
      quotas: this.spent(now),
      now: Math.floor(now / 1e3)
    };
  }
  /** Spent members as `team.pick` wants them: name → Unix seconds when usable again. */
  spent(now) {
    const out = {};
    for (const [name, q] of Object.entries(this.ctx.store.data.quotas)) {
      if (limitedNow(q, now)) out[name] = Math.ceil((q.resetsAt ?? now + LIMIT_MS) / 1e3);
    }
    return out;
  }
  async pass(root) {
    if (!this.meta(root)) return;
    let raw;
    try {
      raw = this.raw(root);
    } catch (error) {
      this.ctx.host.log(`projects: cannot read ${root}: ${errorText(error)}`);
      return;
    }
    if (!(raw.autopilot === true || obj(raw.autopilot).on === true)) return;
    const actions = normalizeActions(this.ctx.core.call("project.next", { root, ...this.stateFor(root) }));
    const briefs = /* @__PURE__ */ new Set();
    for (const action of actions) {
      const k = `${root}\0${action.kind}\0${"role" in action ? action.role : ""}\0${"task" in action ? action.task ?? "" : ""}`;
      switch (action.kind) {
        case "launch": {
          if (this.inflight.has(k) || this.failed.has(k) || this.ctx.store.forRole(root, action.role)) break;
          if (this.ctx.agents.running() >= this.ctx.settings().maxParallel) break;
          this.inflight.add(k);
          void this.launchSpec(root, launchSpec(action, this.ctx.settings().defaultAgent)).catch((error) => {
            this.failed.add(k);
            try {
              this.ctx.core.call("project.stream.failed", { root, role: action.role, reason: errorText(error) });
            } catch {
            }
            this.raiseError(root, action.role, errorText(error));
          }).finally(() => {
            this.inflight.delete(k);
            this.changed(root);
          });
          break;
        }
        case "send": {
          const b = this.ctx.store.forRole(root, action.role);
          if (b?.terminalId) await this.ctx.agents.send(b.terminalId, action.text);
          break;
        }
        case "stop":
          await this.stopRole(root, action.role);
          break;
        case "need": {
          const id = action.needKind === "brief" && action.task ? `brief:${root}:${action.task}` : `project:${root}:${action.role ?? ""}:${action.title}`;
          if (action.needKind === "brief") briefs.add(id);
          this.ctx.needs.add({
            id,
            kind: action.needKind === "brief" ? "brief" : "question",
            title: action.title,
            ...action.detail ? { detail: action.detail } : {},
            createdAt: this.ctx.now(),
            source: { kind: "project", root, ...action.role ? { role: action.role } : {} }
          });
          break;
        }
      }
    }
    this.ctx.needs.removeWhere((n) => n.kind === "brief" && n.source.kind === "project" && n.source.root === root && !briefs.has(n.id));
    this.changed(root);
  }
  raiseError(root, role, message) {
    this.ctx.host.log(`projects: ${root} ${role}: ${message}`);
    this.ctx.needs.add({
      id: `error:${root}:${role}`,
      kind: "error",
      title: T.error(this.ctx.lang(), `${basename(root)} \xB7 ${role}`),
      detail: message,
      createdAt: this.ctx.now(),
      source: { kind: "project", root, role }
    });
  }
  // ---------- agents talking back ----------
  /** `atrium task done|blocked <id> [note]`. */
  taskReport(binding, id, outcome, note) {
    const root = binding.root;
    this.ctx.core.call("project.task.move", { root, id, status: outcome });
    if (note.trim()) this.notice(root, `${id} ${outcome}: ${note.trim()}`, binding.role);
    this.ctx.agents.reported(binding);
    if (outcome === "blocked") {
      this.ctx.needs.add({
        id: `task-blocked:${root}:${id}`,
        kind: "blocked",
        title: this.ctx.lang() === "zh" ? `\u4EFB\u52A1 ${id} \u53D7\u963B` : `Task ${id} is blocked`,
        ...note ? { detail: note } : {},
        createdAt: this.ctx.now(),
        source: { kind: "project", root, ...binding.role ? { role: binding.role } : {} },
        ...binding.terminalId ? { terminalId: binding.terminalId } : {}
      });
    }
    this.changed(root);
    void this.pump(root);
    return this.ctx.lang() === "zh" ? `\u4EFB\u52A1 ${id} \u5DF2\u8BB0\u4E3A ${outcome}\u3002` : `Task ${id} marked ${outcome}.`;
  }
  /** `atrium status` for a role: what is assigned to it. */
  statusText(binding) {
    const root = binding.root;
    const v = this.view(root);
    const mine = v.tasks.filter((t) => t.owner === binding.role && t.status !== "done");
    const head = `${v.name} \xB7 ${binding.role}`;
    if (!mine.length) return `${head}
${this.ctx.lang() === "zh" ? "\uFF08\u6CA1\u6709\u5206\u914D\u7ED9\u4F60\u7684\u4EFB\u52A1\uFF09" : "(no tasks assigned to you)"}`;
    return [head, ...mine.map((t) => `${t.id}  [${t.status}]  ${t.title}`)].join("\n");
  }
  /** `atrium ask`: a question only the person can answer. */
  ask(binding, question) {
    const where = binding.kind === "role" ? `${basename(binding.root ?? "")} \xB7 ${binding.role}` : `${binding.step}${binding.item ? ` / ${binding.item}` : ""}`;
    this.ctx.needs.add({
      id: `ask:${binding.terminalId ?? binding.launchId}:${this.ctx.now()}`,
      kind: "question",
      title: T.question(this.ctx.lang(), where),
      detail: question,
      createdAt: this.ctx.now(),
      source: binding.kind === "role" ? { kind: "project", root: binding.root, ...binding.role ? { role: binding.role } : {} } : { kind: "run", dir: binding.dir, ...binding.step ? { step: binding.step } : {} },
      ...binding.terminalId ? { terminalId: binding.terminalId } : {}
    });
    return this.ctx.lang() === "zh" ? "\u5DF2\u8F6C\u7ED9\u7528\u6237\u3002\u8BF7\u7ED3\u675F\u672C\u8F6E\uFF0C\u7B49\u5F85\u56DE\u590D\u3002" : "Passed to the person. End your turn and wait for the answer.";
  }
  /** A lead's brief waits for the person: any answer but "reject" approves it. */
  approve(need, answer) {
    const root = need.source.root;
    const prefix = root ? `brief:${root}:` : "";
    if (!root || !need.id.startsWith(prefix)) return;
    this.ctx.core.call("project.task.approve", { root, id: need.id.slice(prefix.length), approve: answer !== "reject" });
    this.changed(root);
    void this.pump(root);
  }
  async answer(need, answer) {
    if (need.terminalId && answer) await this.ctx.agents.send(need.terminalId, answerText(this.ctx.lang(), answer));
  }
  onBlocked(binding) {
    const root = binding.root;
    this.ctx.needs.add({
      id: `blocked:${binding.terminalId}`,
      kind: "blocked",
      title: T.blocked(this.ctx.lang(), `${basename(root)} \xB7 ${binding.role}`),
      createdAt: this.ctx.now(),
      source: { kind: "project", root, ...binding.role ? { role: binding.role } : {} },
      ...binding.terminalId ? { terminalId: binding.terminalId } : {}
    });
  }
  onGone(binding) {
    if (binding.root) {
      this.changed(binding.root);
      void this.pump(binding.root);
    }
  }
  /** A role agent hit its limit: the core places a stand-in, or the person is asked. */
  async onQuota(binding, evidence) {
    const root = binding.root;
    const role = binding.role;
    const member = binding.member ?? this.teamGet(root).members.find((m) => m.agent === binding.agent)?.name ?? binding.agent;
    this.markLimited(member);
    let spec = null;
    try {
      spec = this.plan(root, role);
    } catch (error) {
      this.ctx.host.log(`projects: no stand-in plan for ${role}: ${errorText(error)}`);
    }
    const standIn = spec && spec.standingInFor && spec.member !== member ? spec : null;
    this.ctx.needs.add({
      id: `quota:${binding.terminalId}`,
      kind: "quota",
      title: T.quota(this.ctx.lang(), member, standIn?.member),
      detail: evidence,
      createdAt: this.ctx.now(),
      source: { kind: "project", root, role },
      ...binding.terminalId ? { terminalId: binding.terminalId } : {}
    });
    if (!standIn) return;
    await this.ctx.agents.release(binding, true);
    try {
      await this.launchSpec(root, { ...standIn, standingInFor: member, ...binding.task && !standIn.task ? { task: binding.task } : {} });
    } catch (error) {
      this.raiseError(root, role, errorText(error));
    }
    this.changed(root);
  }
  markLimited(member) {
    const now = this.ctx.now();
    const old = this.ctx.store.data.quotas[member];
    this.ctx.store.data.quotas[member] = { ...old ?? {}, limited: true, resetsAt: old?.resetsAt && old.resetsAt > now ? old.resetsAt : now + LIMIT_MS };
    this.ctx.store.save();
  }
  /** `team.pick` for `wanted`, with the members spent now skipped. */
  pickFor(root, wanted) {
    try {
      const raw = this.ctx.core.call("team.pick", { root, wanted, quotas: this.spent(this.ctx.now()), now: Math.floor(this.ctx.now() / 1e3) });
      return normalizePick(raw);
    } catch (error) {
      this.ctx.host.log(`projects: team.pick: ${errorText(error)}`);
      return null;
    }
  }
  /** For runs: a stand-in backend from the team of the project that holds the run folder. */
  standInFor(dir, binding) {
    const meta = this.metas().find((p) => contains(p.root, dir));
    if (!meta) return null;
    const members = this.teamGet(meta.root).members;
    const limited = members.filter((m) => m.agent === binding.agent);
    for (const m of limited) this.markLimited(m.name);
    const wanted = binding.member ?? limited[0]?.name;
    if (!wanted) return null;
    const pick = this.pickFor(meta.root, wanted);
    if (!pick || pick.member === wanted) return null;
    const agent = pick.agent ?? members.find((m) => m.name === pick.member)?.agent;
    return agent && agent !== binding.agent ? { agent, member: pick.member } : null;
  }
  quotaOf(member) {
    return this.ctx.store.data.quotas[member];
  }
};
function limitedNow(q, now) {
  return q.limited === true && (q.resetsAt === void 0 || q.resetsAt > now);
}

// worker/src/engine/runs.ts
import { existsSync as existsSync4 } from "node:fs";
import { basename as basename2, join as join4 } from "node:path";

// worker/src/engine/checks.ts
import { spawn } from "node:child_process";
import { readFileSync as readFileSync2 } from "node:fs";
import { isAbsolute as isAbsolute2, relative as relative2, resolve as resolve2 } from "node:path";
import { inflateSync } from "node:zlib";
var LOG_CAP = 16 * 1024;
function insideDir(dir, path) {
  const rel = relative2(resolve2(dir), resolve2(path));
  return rel === "" || !rel.startsWith("..") && !isAbsolute2(rel);
}
function runCommand(opts) {
  const cwd = resolve2(opts.dir, opts.cwd ?? ".");
  if (!insideDir(opts.dir, cwd)) {
    return Promise.resolve({ code: null, timedOut: false, log: `cwd ${opts.cwd} is outside the run folder` });
  }
  const win = process.platform === "win32";
  return new Promise((done) => {
    let log = "";
    const keep = (chunk) => {
      log += chunk.toString("utf8");
      if (log.length > LOG_CAP * 2) log = log.slice(-LOG_CAP);
    };
    let child;
    try {
      child = spawn(win ? process.env.ComSpec || "cmd.exe" : "/bin/sh", win ? ["/d", "/s", "/c", opts.command] : ["-c", opts.command], {
        cwd,
        env: { ...process.env, ...opts.env },
        detached: !win,
        windowsHide: true,
        stdio: ["ignore", "pipe", "pipe"]
      });
    } catch (error) {
      done({ code: null, timedOut: false, log: String(error) });
      return;
    }
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      killTree(child);
    }, opts.timeoutMs);
    child.stdout?.on("data", keep);
    child.stderr?.on("data", keep);
    child.on("error", (error) => {
      clearTimeout(timer);
      done({ code: null, timedOut, log: `${log}
${String(error)}`.slice(-LOG_CAP) });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      done({ code, timedOut, log: log.slice(-LOG_CAP) });
    });
  });
}
function killTree(child) {
  if (child.pid === void 0) return;
  if (process.platform === "win32") {
    try {
      spawn("taskkill", ["/pid", String(child.pid), "/t", "/f"], { windowsHide: true, stdio: "ignore" });
    } catch {
    }
    return;
  }
  try {
    process.kill(-child.pid, "SIGKILL");
  } catch {
    try {
      child.kill("SIGKILL");
    } catch {
    }
  }
}
function pdfPageCount(bytes) {
  if (bytes.subarray(0, 5).toString("latin1") !== "%PDF-") return null;
  const texts = [bytes.toString("latin1")];
  const raw = texts[0];
  const streamRe = /stream\r?\n/g;
  let m;
  let scanned = 0;
  while ((m = streamRe.exec(raw)) && scanned < 5e3) {
    const start = m.index + m[0].length;
    const end = raw.indexOf("endstream", start);
    if (end < 0) break;
    const header = raw.slice(Math.max(0, m.index - 300), m.index);
    if (/\/ObjStm/.test(header) && /\/FlateDecode/.test(header)) {
      try {
        texts.push(inflateSync(bytes.subarray(start, end)).toString("latin1"));
      } catch {
      }
    }
    streamRe.lastIndex = end;
    scanned++;
  }
  let pages = 0;
  let maxCount = 0;
  for (const text2 of texts) {
    pages += (text2.match(/\/Type\s*\/Page(?![a-zA-Z])/g) ?? []).length;
    for (const c of text2.matchAll(/\/Type\s*\/Pages\b[^>]*?\/Count\s+(\d+)|\/Count\s+(\d+)[^>]*?\/Type\s*\/Pages\b/g)) {
      maxCount = Math.max(maxCount, Number(c[1] ?? c[2]));
    }
  }
  if (pages > 0) return pages;
  if (maxCount > 0) return maxCount;
  return null;
}
function num(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : void 0;
}
function describeCheck(check) {
  if (typeof check.what === "string" && check.what) return check.what;
  switch (check.kind) {
    case "command":
      return `\`${String(check.run ?? check.command ?? "")}\` succeeds`;
    case "pdf_pages": {
      const path = String(check.path ?? "");
      const min = num(check.min);
      const max = num(check.max);
      if (min !== void 0 && max !== void 0) return `${path} has ${min}\u2013${max} pages`;
      if (max !== void 0) return `${path} has at most ${max} pages`;
      if (min !== void 0) return `${path} has at least ${min} pages`;
      return `${path} is a valid PDF`;
    }
    default:
      return String(check.kind);
  }
}
async function runCheck(dir, check) {
  const what = describeCheck(check);
  if (check.kind === "command") {
    const command = String(check.run ?? check.command ?? "");
    if (!command) return { what, passed: false, detail: "no command" };
    const minutes = num(check.timeout_min) ?? num(check.timeoutMin) ?? 5;
    const timeoutMs = num(check.timeout_ms) ?? minutes * 6e4;
    const out = await runCommand({ command, dir, ...typeof check.cwd === "string" ? { cwd: check.cwd } : {}, timeoutMs });
    const passed = out.code === 0 && !out.timedOut;
    const tail = out.log.trim().split("\n").slice(-20).join("\n");
    const message = typeof check.message === "string" ? check.message : "";
    const detail = passed ? "" : out.timedOut ? `timed out after ${Math.round(timeoutMs / 1e3)} s` : [message, out.code === null ? "could not run" : `exit code ${out.code}`, tail].filter(Boolean).join("\n");
    return { what, passed, detail, log: out.log };
  }
  if (check.kind === "pdf_pages") {
    const path = resolve2(dir, String(check.path ?? ""));
    if (!insideDir(dir, path)) return { what, passed: false, detail: "the path is outside the run folder" };
    let bytes;
    try {
      bytes = readFileSync2(path);
    } catch (error) {
      return { what, passed: false, detail: error.message };
    }
    const count = pdfPageCount(bytes);
    if (count === null) return { what, passed: false, detail: "not a PDF, or no page count" };
    const min = num(check.min);
    const max = num(check.max);
    const passed = (max === void 0 || count <= max) && (min === void 0 || count >= min);
    return { what, passed, detail: passed ? "" : `has ${count} pages` };
  }
  return { what, passed: false, detail: `the worker cannot run a ${check.kind} check` };
}
function readVerdict(report) {
  const text2 = report.trim();
  const passed = /^(pass|通过)(?![a-z])/i.test(text2);
  return { passed, detail: passed ? "" : text2.replace(/^(FAIL|不通过)[:：]?\s*/i, "") };
}

// worker/src/engine/runs.ts
var key = (dir, step, item) => `${dir}\0${step}\0${item ?? ""}`;
var FINAL = /* @__PURE__ */ new Set(["done", "failed", "cancelled"]);
var Runs = class {
  constructor(ctx) {
    this.ctx = ctx;
  }
  chains = /* @__PURE__ */ new Map();
  inflight = /* @__PURE__ */ new Set();
  /** Launches that failed: not retried until the person asks. */
  failed = /* @__PURE__ */ new Set();
  /** Agent overrides after a stand-in pick. */
  agentFor = /* @__PURE__ */ new Map();
  finalCache = /* @__PURE__ */ new Map();
  standIns = null;
  metas() {
    return this.ctx.store.data.runs;
  }
  meta(dir) {
    return this.ctx.store.data.runs.find((r) => r.dir === dir);
  }
  requireMeta(dir) {
    const meta = this.meta(dir);
    if (!meta) throw new HostError("invalid_params", `unknown run ${dir}`);
    return meta;
  }
  // ---------- start ----------
  async start(params) {
    const ws = params.workspaceId ? await this.ctx.workspaces.byId(params.workspaceId) : await this.ctx.workspaces.focusedWorkspace();
    if (!ws) throw new HostError("invalid_params", T.noWorkspace(this.ctx.lang()));
    const templates = this.ctx.core.call("workflow.templates", { dirs: this.ctx.templateDirs() });
    const template = (Array.isArray(templates) ? templates : []).find((t) => t.id === params.templateId);
    const dir = freeDir(join4(ws.path, "atrium-runs"), `${safeName(params.templateId)}-${stamp(new Date(this.ctx.now()))}`);
    this.ctx.core.call("workflow.run.start", {
      template: params.templateId,
      ...template?.path ? { path: template.path } : {},
      dirs: this.ctx.templateDirs(),
      inputs: params.inputs ?? {},
      dir
    });
    this.ctx.store.data.runs.unshift({ dir, workspaceId: ws.id, templateId: params.templateId });
    this.ctx.store.save();
    this.ctx.host.post("overview.changed");
    void this.pump(dir);
    return { dir };
  }
  // ---------- the loop ----------
  /** One pass over what the run needs, serialized per run. */
  pump(dir) {
    const prev = this.chains.get(dir) ?? Promise.resolve();
    const next = prev.then(() => this.pass(dir)).catch((error) => this.ctx.host.log(`runs: ${dir}: ${errorText(error)}`));
    this.chains.set(dir, next);
    return next;
  }
  /** Every run that is not finished. */
  async tick() {
    await Promise.all(this.metas().filter((m) => !this.finalCache.has(m.dir)).map((m) => this.pump(m.dir)));
  }
  /** Resolves once every queued pass and started launch/check has settled (for tests). */
  async settle() {
    for (let i = 0; i < 50; i++) {
      await Promise.all([...this.chains.values()]);
      await new Promise((r) => setTimeout(r, 5));
      if (this.inflight.size === 0) {
        await Promise.all([...this.chains.values()]);
        return;
      }
    }
  }
  async pass(dir) {
    const meta = this.meta(dir);
    if (!meta) return;
    let effects;
    try {
      effects = normalizeEffects(this.ctx.core.call("workflow.run.next", { dir, lang: this.ctx.lang(), maxParallel: this.ctx.settings().maxParallel }));
    } catch (error) {
      this.raiseError(dir, void 0, errorText(error));
      return;
    }
    for (const effect of effects) {
      switch (effect.kind) {
        case "launch":
          this.maybeLaunch(meta, effect);
          break;
        case "check":
          this.maybeCheck(meta, effect);
          break;
        case "gate":
          this.raiseGate(meta, effect);
          break;
        case "done":
          this.finalCache.set(dir, effect.state ?? "done");
          this.ctx.needs.removeWhere((n) => n.source.kind === "run" && n.source.dir === dir && n.kind === "gate");
          break;
        case "wait":
          break;
      }
    }
    this.ctx.host.post("run.changed", { dir });
    this.ctx.host.post("overview.changed");
  }
  maybeLaunch(meta, e) {
    const k = key(meta.dir, e.step, e.item);
    if (this.inflight.has(k) || this.failed.has(k) || this.ctx.store.forStep(meta.dir, e.step, e.item)) return;
    if (this.ctx.agents.running() >= this.ctx.settings().maxParallel) return;
    this.inflight.add(k);
    void this.launchStep(meta, e).then(() => {
      this.inflight.delete(k);
    }).catch((error) => {
      this.inflight.delete(k);
      this.failed.add(k);
      this.raiseError(meta.dir, e.step, errorText(error));
    }).finally(() => void this.pump(meta.dir));
  }
  async launchStep(meta, e) {
    const k = key(meta.dir, e.step, e.item);
    let workspaceId = meta.workspaceId;
    if (e.worktree) {
      const base = await this.ctx.workspaces.byId(meta.workspaceId);
      if (!base) throw new Error(`workspace ${meta.workspaceId} is gone`);
      workspaceId = await this.ctx.workspaces.createWorktree(base, `${meta.templateId}-${e.step}${e.item ? `-${e.item}` : ""}`);
    }
    const agent = this.agentFor.get(k) ?? e.agent ?? this.ctx.settings().defaultAgent;
    const prompt = `${e.prompt}
${stepFooter(this.ctx.lang(), { dir: meta.dir, step: e.step, ...e.item ? { item: e.item } : {}, cli: this.ctx.cli })}`;
    return this.ctx.agents.launch({
      workspaceId,
      agent,
      prompt,
      binding: { kind: "step", dir: meta.dir, step: e.step, ...e.item ? { item: e.item } : {} },
      env: { ATRIUM_RUN: meta.dir, ATRIUM_STEP: e.step, ...e.item ? { ATRIUM_ITEM: e.item } : {} }
    });
  }
  maybeCheck(meta, e) {
    const k = `check\0${key(meta.dir, e.step, e.item)}\0${e.index ?? ""}`;
    if (this.inflight.has(k) || this.failed.has(k)) return;
    if (e.check.kind === "review") {
      const open = this.ctx.store.live().some((b) => b.kind === "review" && b.dir === meta.dir && b.step === e.step && (b.item ?? "") === (e.item ?? ""));
      if (open) return;
      this.inflight.add(k);
      void this.launchReview(meta, e).catch((error) => this.sendCheck(meta.dir, e, { what: "review", passed: false, detail: errorText(error) })).finally(() => this.inflight.delete(k));
      return;
    }
    this.inflight.add(k);
    void runCheck(meta.dir, e.check).catch((error) => ({ what: String(e.check.kind), passed: false, detail: errorText(error) })).then((result) => {
      if (!this.sendCheck(meta.dir, e, result)) this.failed.add(k);
    }).finally(() => {
      this.inflight.delete(k);
      void this.pump(meta.dir);
    });
  }
  sendCheck(dir, e, result) {
    try {
      this.ctx.core.call("workflow.run.check_result", {
        dir,
        step: e.step,
        ...e.item ? { item: e.item } : {},
        ...e.index !== void 0 ? { index: e.index } : {},
        lang: this.ctx.lang(),
        result
      });
      return true;
    } catch (error) {
      this.raiseError(dir, e.step, errorText(error));
      return false;
    }
  }
  async launchReview(meta, e) {
    const check = e.check;
    const lang = this.ctx.lang();
    const prompt = typeof check.prompt === "string" && check.prompt || reviewPrompt(lang, String(check.rubric ?? ""), Array.isArray(check.files) ? check.files.map(String) : [], meta.dir);
    await this.ctx.agents.launch({
      workspaceId: meta.workspaceId,
      agent: typeof check.agent === "string" && check.agent ? check.agent : this.ctx.settings().defaultAgent,
      prompt: `${prompt}
${stepFooter(lang, { dir: meta.dir, step: e.step, ...e.item ? { item: e.item } : {}, cli: this.ctx.cli })}`,
      binding: { kind: "review", dir: meta.dir, step: e.step, ...e.item ? { item: e.item } : {}, ...e.index !== void 0 ? { checkIndex: e.index } : {} },
      env: { ATRIUM_RUN: meta.dir, ATRIUM_STEP: e.step, ATRIUM_REVIEW: "1", ...e.item ? { ATRIUM_ITEM: e.item } : {} }
    });
  }
  raiseGate(meta, e) {
    const id = `gate:${meta.dir}:${e.step}`;
    if (this.ctx.needs.get(id)) return;
    const name = this.templateName(meta);
    this.ctx.needs.add({
      id,
      kind: "gate",
      title: e.title ?? T.gate(this.ctx.lang(), `${name} \xB7 ${e.step}`),
      ...e.detail ? { detail: e.detail } : {},
      createdAt: this.ctx.now(),
      source: { kind: "run", dir: meta.dir, step: e.step },
      choices: e.choices.length ? e.choices : [{ id: "approve", label: this.ctx.lang() === "zh" ? "\u901A\u8FC7" : "Approve" }],
      sendBackTargets: e.sendBack
    });
  }
  raiseError(dir, step, message) {
    this.ctx.host.log(`runs: ${dir}${step ? ` ${step}` : ""}: ${message}`);
    const meta = this.meta(dir);
    this.ctx.needs.add({
      id: `error:${dir}:${step ?? ""}`,
      kind: "error",
      title: T.error(this.ctx.lang(), `${meta ? this.templateName(meta) : basename2(dir)}${step ? ` \xB7 ${step}` : ""}`),
      detail: message,
      createdAt: this.ctx.now(),
      source: { kind: "run", dir, ...step ? { step } : {} }
    });
  }
  templateName(meta) {
    try {
      const view = this.ctx.core.call("workflow.run.load", { dir: meta.dir, lang: this.ctx.lang() });
      return view?.templateName || meta.templateId;
    } catch {
      return meta.templateId;
    }
  }
  // ---------- agents talking back ----------
  /** `atrium report` from a step or review agent. */
  async report(binding, text2) {
    const dir = binding.dir;
    const step = binding.step;
    this.ctx.agents.reported(binding);
    if (binding.kind === "review") {
      const verdict = readVerdict(text2);
      this.sendCheck(dir, { step, ...binding.item ? { item: binding.item } : {}, ...binding.checkIndex !== void 0 ? { index: binding.checkIndex } : {} }, {
        what: "review",
        passed: verdict.passed,
        detail: verdict.detail
      });
    } else {
      this.ctx.core.call("workflow.run.report", { dir, step, ...binding.item ? { item: binding.item } : {}, answer: text2 });
    }
    await this.ctx.agents.release(binding, false);
    this.ctx.needs.removeWhere((n) => n.terminalId !== void 0 && n.terminalId === binding.terminalId && (n.kind === "stalled" || n.kind === "blocked"));
    await this.pump(dir);
    return this.ctx.lang() === "zh" ? "\u5DF2\u6536\u5230\u62A5\u544A\u3002" : "Report received.";
  }
  onStalled(binding) {
    const meta = binding.dir ? this.meta(binding.dir) : void 0;
    const what = `${meta ? this.templateName(meta) : basename2(binding.dir ?? "")} \xB7 ${binding.step}${binding.item ? ` / ${binding.item}` : ""}`;
    this.ctx.needs.add({
      id: `stalled:${binding.terminalId}`,
      kind: "stalled",
      title: T.stalled(this.ctx.lang(), what),
      createdAt: this.ctx.now(),
      source: { kind: "run", dir: binding.dir, ...binding.step ? { step: binding.step } : {} },
      ...binding.terminalId ? { terminalId: binding.terminalId } : {}
    });
  }
  onBlocked(binding) {
    this.ctx.needs.add({
      id: `blocked:${binding.terminalId}`,
      kind: "blocked",
      title: T.blocked(this.ctx.lang(), `${binding.step}${binding.item ? ` / ${binding.item}` : ""}`),
      createdAt: this.ctx.now(),
      source: { kind: "run", dir: binding.dir, ...binding.step ? { step: binding.step } : {} },
      ...binding.terminalId ? { terminalId: binding.terminalId } : {}
    });
  }
  /** The terminal went away before the agent reported: start the step again. */
  onGone(binding) {
    if (binding.reported || !binding.dir || !binding.step) return;
    this.retryInCore(binding.dir, binding.step, binding.item);
    void this.pump(binding.dir);
  }
  /** Usage limit: a stand-in from the project's team, else ask the person. */
  async onQuota(binding, evidence) {
    if (!binding.dir || !binding.step) return;
    const pick = this.standIns?.standInFor(binding.dir, binding) ?? null;
    this.ctx.needs.add({
      id: `quota:${binding.terminalId}`,
      kind: "quota",
      title: T.quota(this.ctx.lang(), binding.member ?? binding.agent, pick?.member),
      detail: evidence,
      createdAt: this.ctx.now(),
      source: { kind: "run", dir: binding.dir, step: binding.step },
      ...binding.terminalId ? { terminalId: binding.terminalId } : {}
    });
    if (!pick) return;
    this.agentFor.set(key(binding.dir, binding.step, binding.item), pick.agent);
    await this.ctx.agents.release(binding, false);
    this.retryInCore(binding.dir, binding.step, binding.item);
    await this.pump(binding.dir);
  }
  retryInCore(dir, step, item) {
    try {
      this.ctx.core.call("workflow.run.retry", { dir, step, ...item ? { item } : {} });
    } catch (error) {
      this.ctx.host.log(`runs: retry ${step} in ${dir}: ${errorText(error)}`);
    }
  }
  // ---------- requests ----------
  async decide(p) {
    this.requireMeta(p.dir);
    this.ctx.core.call("workflow.run.decide", {
      dir: p.dir,
      step: p.step,
      choice: p.choice,
      ...p.sendBackTo ? { send_back_to: p.sendBackTo } : {},
      ...p.note ? { note: p.note } : {}
    });
    this.ctx.needs.remove(`gate:${p.dir}:${p.step}`);
    await this.pump(p.dir);
  }
  async control(dir, op) {
    this.requireMeta(dir);
    this.ctx.core.call(`workflow.run.${op}`, { dir });
    if (op === "cancel") {
      for (const b of this.ctx.store.live().filter((b2) => b2.dir === dir && b2.kind !== "role")) await this.ctx.agents.release(b, true);
      this.ctx.needs.removeWhere((n) => n.source.kind === "run" && n.source.dir === dir);
    }
    if (op === "resume") this.finalCache.delete(dir);
    await this.pump(dir);
  }
  async retry(p) {
    this.requireMeta(p.dir);
    const k = key(p.dir, p.step, p.item);
    this.failed.delete(k);
    for (const f of [...this.failed]) if (f.startsWith(`check\0${k}`)) this.failed.delete(f);
    const old = this.ctx.store.forStep(p.dir, p.step, p.item);
    if (old) await this.ctx.agents.release(old, false);
    this.ctx.core.call("workflow.run.retry", { dir: p.dir, step: p.step, ...p.item ? { item: p.item } : {} });
    this.ctx.needs.removeWhere((n) => n.source.kind === "run" && n.source.dir === p.dir && n.source.step === p.step && n.kind !== "gate");
    this.finalCache.delete(p.dir);
    await this.pump(p.dir);
  }
  view(dir) {
    const meta = this.requireMeta(dir);
    const raw = this.ctx.core.call("workflow.run.load", { dir, lang: this.ctx.lang() });
    const steps = (raw.steps ?? []).map((s) => {
      const items = s.items;
      const bound = this.ctx.store.forStep(dir, s.id) ?? this.ctx.store.live().find((b) => b.dir === dir && b.step === s.id);
      return {
        ...s,
        needs: s.needs ?? [],
        attempts: s.attempts ?? [],
        ...items ? { items } : {},
        ...s.terminalId || bound?.terminalId ? { terminalId: s.terminalId ?? bound.terminalId } : {}
      };
    });
    const view = {
      dir,
      templateId: raw.templateId ?? meta.templateId,
      templateName: raw.templateName ?? meta.templateId,
      state: raw.state ?? "running",
      startedAt: raw.startedAt ?? 0,
      progress: raw.progress ?? { done: steps.filter((s) => s.status === "passed" || s.status === "skipped").length, total: steps.length },
      ...raw.currentStep ? { currentStep: raw.currentStep } : {},
      inputs: raw.inputs ?? {},
      steps,
      decisions: raw.decisions ?? [],
      experience: raw.experience ?? []
    };
    if (FINAL.has(view.state)) this.finalCache.set(dir, view.state);
    return view;
  }
  summary(dir) {
    try {
      const v = this.view(dir);
      return {
        dir: v.dir,
        templateId: v.templateId,
        templateName: v.templateName,
        state: v.state,
        startedAt: v.startedAt,
        progress: v.progress,
        ...v.currentStep ? { currentStep: v.currentStep } : {}
      };
    } catch (error) {
      this.ctx.host.log(`runs: cannot load ${dir}: ${errorText(error)}`);
      return null;
    }
  }
};
function stamp(d) {
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}
function freeDir(parent, name) {
  let dir = join4(parent, name);
  for (let n = 2; existsSync4(dir); n++) dir = join4(parent, `${name}-${n}`);
  return dir;
}

// worker/src/links.ts
import { existsSync as existsSync5, mkdirSync as mkdirSync2, readFileSync as readFileSync3, writeFileSync } from "node:fs";
import { join as join5 } from "node:path";
var BOARD_FILE = "atrium.board";
var RUN_FILE = "view.atrium-run";
function ignore(atriumDir, name) {
  const path = join5(atriumDir, ".gitignore");
  const text2 = existsSync5(path) ? readFileSync3(path, "utf8") : "";
  const lines = text2.split(/\r?\n/);
  if (lines.includes(name)) return;
  writeFileSync(path, `${text2}${text2 && !text2.endsWith("\n") ? "\n" : ""}${name}
`);
}
function writeLink(base, file, body) {
  const dir = join5(base, ".atrium");
  mkdirSync2(dir, { recursive: true });
  const path = join5(dir, file);
  const text2 = `${JSON.stringify(body, null, 2)}
`;
  if (!existsSync5(path) || readFileSync3(path, "utf8") !== text2) writeFileSync(path, text2);
  ignore(dir, file);
  return path;
}
function writeBoardLink(root) {
  return writeLink(root, BOARD_FILE, { project: root });
}
function writeRunLink(dir) {
  return writeLink(dir, RUN_FILE, { run: dir });
}
function readLink(text2) {
  let value;
  try {
    value = JSON.parse(text2);
  } catch {
    throw new HostError("invalid_params", "not an Atrium link file");
  }
  const v = value;
  if (typeof v.project === "string" && v.project) return { kind: "board", root: v.project };
  if (typeof v.run === "string" && v.run) return { kind: "run", dir: v.run };
  throw new HostError("invalid_params", "not an Atrium link file");
}
async function openLink(host, workspaces, path, preferred) {
  const first = preferred ? await workspaces.byId(preferred) : void 0;
  const ws = first && contains(first.path, path) ? first : await workspaces.forPath(path);
  if (!ws) throw new HostError("action_failed", `no Orca workspace holds ${path}`);
  await host.call("editor.open", { workspaceId: ws.id, path: workspaces.relative(ws, path) });
}

// worker/src/settings.ts
var DEFAULTS = { language: "auto", defaultAgent: "claude", maxParallel: 4, autopilotDefault: false };
function parseSettings(raw) {
  const r = raw ?? {};
  const language = r.language === "zh" || r.language === "en" ? r.language : "auto";
  const defaultAgent = typeof r.defaultAgent === "string" && r.defaultAgent.trim() ? r.defaultAgent.trim() : DEFAULTS.defaultAgent;
  const maxParallel = typeof r.maxParallel === "number" && Number.isFinite(r.maxParallel) && r.maxParallel >= 1 ? Math.floor(r.maxParallel) : DEFAULTS.maxParallel;
  const autopilotDefault = typeof r.autopilotDefault === "boolean" ? r.autopilotDefault : DEFAULTS.autopilotDefault;
  return { language, defaultAgent, maxParallel, autopilotDefault };
}
async function readSettings(host) {
  try {
    return parseSettings(await host.settings());
  } catch {
    return { ...DEFAULTS };
  }
}

// worker/src/state.ts
import { randomBytes } from "node:crypto";
var KEY = "atrium.state";
var Store = class _Store {
  constructor(host, data) {
    this.host = host;
    this.data = data;
  }
  data;
  timer = null;
  writing = Promise.resolve();
  static async load(host) {
    let raw = null;
    try {
      raw = await host.storageGet(KEY);
    } catch (error) {
      host.log(`state: could not read storage: ${String(error)}`);
    }
    const data = {
      token: typeof raw?.token === "string" && raw.token ? raw.token : randomBytes(18).toString("hex"),
      projects: Array.isArray(raw?.projects) ? raw.projects : [],
      runs: Array.isArray(raw?.runs) ? raw.runs : [],
      needs: Array.isArray(raw?.needs) ? raw.needs : [],
      bindings: Array.isArray(raw?.bindings) ? raw.bindings : [],
      quotas: raw?.quotas && typeof raw.quotas === "object" ? raw.quotas : {}
    };
    const store = new _Store(host, data);
    if (!raw?.token) store.save();
    return store;
  }
  /** Coalesced write. */
  save() {
    if (this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.flush();
    }, 50);
  }
  async flush() {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    const snapshot = JSON.parse(JSON.stringify(this.data));
    this.writing = this.writing.then(() => this.host.storageSet(KEY, snapshot)).catch((error) => this.host.log(`state: could not write storage: ${String(error)}`));
    await this.writing;
  }
  // ---------- bindings ----------
  live() {
    return this.data.bindings.filter((b) => !b.ended);
  }
  byTerminal(terminalId) {
    return this.live().find((b) => b.terminalId === terminalId);
  }
  byLaunch(launchId) {
    return this.data.bindings.find((b) => b.launchId === launchId);
  }
  forStep(dir, step, item) {
    return this.live().find((b) => b.kind === "step" && b.dir === dir && b.step === step && (b.item ?? "") === (item ?? ""));
  }
  forRole(root, role) {
    return this.live().find((b) => b.kind === "role" && b.root === root && b.role === role);
  }
  addBinding(b) {
    this.data.bindings.push(b);
    const ended = this.data.bindings.filter((x) => x.ended);
    if (ended.length > 200) {
      const drop = new Set(ended.slice(0, ended.length - 200));
      this.data.bindings = this.data.bindings.filter((x) => !drop.has(x));
    }
    this.save();
  }
  end(b) {
    b.ended = true;
    this.save();
  }
};

// worker/src/service.ts
var Service = class _Service {
  constructor(host, opts) {
    this.host = host;
    this.opts = opts;
  }
  store;
  ctx;
  runs;
  projects;
  needs;
  agents;
  bridge;
  workspaces;
  current = { ...DEFAULTS };
  disposables = [];
  timer = null;
  static async create(host, opts) {
    const s = new _Service(host, opts);
    await s.init();
    return s;
  }
  async init() {
    const { host, opts } = this;
    this.current = await readSettings(host);
    const store = await Store.load(host);
    const workspaces = new Workspaces(host);
    const lang = () => resolveLang(this.current.language);
    const hook = new LaunchHook(opts.binDir);
    const agents = new Agents(host, store, hook, { graceMs: opts.graceMs ?? 45e3, socket: opts.socket, token: store.data.token }, lang);
    const needs = new Needs(host, store, lang);
    const ctx = {
      host,
      core: opts.core,
      store,
      agents,
      needs,
      workspaces,
      settings: () => this.current,
      lang,
      cli: opts.cli,
      templateDirs: () => this.templateDirs(),
      now: opts.now ?? (() => Date.now())
    };
    const runs = new Runs(ctx);
    const projects = new Projects(ctx);
    runs.standIns = projects;
    needs.countRunning(() => agents.running());
    Object.assign(this, { store, ctx, runs, projects, needs, agents, workspaces });
    Object.assign(this, { bridge: new BridgeServer(opts.socket, store.data.token, makeCliHandler(ctx, runs, projects), (m) => host.log(m)) });
    this.wireNeeds();
    this.wireAgents();
    await this.bridge.start();
    this.disposables.push(hook.register(host));
    this.wireEvents();
    this.wireRequests();
    this.wireCommands();
    this.disposables.push(host.onSettings(() => void readSettings(host).then((s) => (this.current = s, void needs.updateStatusBar()))));
    await agents.reconcile();
    await needs.updateStatusBar();
    void this.tick();
    this.timer = setInterval(() => void this.tick(), opts.tickMs ?? 15e3);
    this.timer.unref?.();
  }
  /** Resumes anything that waits: runs, project autopilots. */
  async tick() {
    try {
      await this.projects.discover();
      await Promise.all([this.runs.tick(), this.projects.tick()]);
      await this.needs.updateStatusBar();
    } catch (error) {
      this.host.log(`tick: ${errorText(error)}`);
    }
  }
  /** Waits until everything started has settled (tests). */
  async settle() {
    await this.runs.settle();
    await this.projects.settle();
    await this.runs.settle();
  }
  async dispose() {
    if (this.timer) clearInterval(this.timer);
    for (const d of this.disposables.splice(0)) {
      try {
        d.dispose();
      } catch {
      }
    }
    this.agents.dispose();
    await this.bridge.stop();
    await this.store.flush();
  }
  templateDirs() {
    const dirs = [this.opts.userTemplates ?? join6(homedir(), ".config", "Atrium", "workflows")];
    for (const p of this.store.data.projects) dirs.push(join6(p.root, ".atrium", "workflows"));
    const focused = this.workspaces.focused?.workspaceId;
    const ws = focused ? this.workspaces.cached(focused) : void 0;
    if (ws) dirs.push(join6(ws.path, ".atrium", "workflows"));
    return [...new Set(dirs)];
  }
  // ---------- wiring ----------
  wireNeeds() {
    const { needs, runs, projects } = this;
    needs.onResolve("gate", async (need, answer) => {
      if (need.source.kind !== "run" || !need.source.step) return;
      await runs.decide({ dir: need.source.dir, step: need.source.step, choice: answer || need.choices?.[0]?.id || "approve" });
    });
    needs.onResolve("brief", (need, answer) => projects.approve(need, answer));
    needs.onResolve("question", (need, answer) => projects.answer(need, answer ?? ""));
  }
  wireAgents() {
    const { agents, runs, projects, needs, host } = this;
    agents.events = {
      changed: (binding, state, terminalId) => {
        if (state === "working") needs.removeWhere((n) => n.terminalId === terminalId && (n.kind === "blocked" || n.kind === "stalled"));
        if (binding?.dir) host.post("run.changed", { dir: binding.dir });
        if (binding?.root) host.post("project.changed", { root: binding.root });
        void needs.updateStatusBar();
      },
      stalled: (b) => b.kind === "role" ? void 0 : runs.onStalled(b),
      blocked: (b) => b.kind === "role" ? projects.onBlocked(b) : runs.onBlocked(b),
      idle: (b) => {
        if (b.kind === "role") projects.wake(b, "turn_ended");
      },
      quota: (b, evidence) => void (b.kind === "role" ? projects.onQuota(b, evidence) : runs.onQuota(b, evidence)),
      gone: (b) => b.kind === "role" ? projects.onGone(b) : runs.onGone(b)
    };
  }
  wireEvents() {
    const { host, agents, workspaces } = this;
    this.disposables.push(
      host.on("agent.status.changed", (p) => agents.onStatus(p)),
      host.on("workspace.changed", (p) => {
        workspaces.focused = p?.context ?? null;
      }),
      host.on("workspace.created", () => workspaces.invalidate()),
      host.on("workspace.removed", () => workspaces.invalidate()),
      host.hook("workspace.create.after", () => {
        workspaces.invalidate();
        return void 0;
      })
    );
  }
  wireCommands() {
    const { host, workspaces, projects } = this;
    const notify = (body) => host.call("notifications.show", { title: "Atrium", body }).catch(() => {
    });
    const lang = () => this.ctx.lang();
    this.disposables.push(
      host.command("atrium.open", () => notify(T.openHint(lang()))),
      host.command("atrium.startWorkflow", () => notify(T.pickTemplate(lang()))),
      host.command("atrium.newProject", async () => {
        const { root } = await projects.init({});
        await this.openBoard(root);
      }),
      host.command("atrium.openBoard", async () => {
        const ws = await workspaces.focusedWorkspace();
        if (!ws) return notify(T.noWorkspace(lang()));
        if (!projects.meta(ws.path)) await projects.discover();
        if (!projects.meta(ws.path)) await projects.init({ root: ws.path });
        await this.openBoard(ws.path);
      })
    );
  }
  async openBoard(root) {
    const meta = this.projects.meta(root);
    await openLink(this.host, this.workspaces, writeBoardLink(root), meta?.workspaceId);
  }
  async openRun(dir) {
    const meta = this.runs.meta(dir);
    if (!meta) throw new HostError("invalid_params", `unknown run ${dir}`);
    await openLink(this.host, this.workspaces, writeRunLink(dir), meta.workspaceId);
  }
  templates() {
    const raw = this.ctx.core.call("workflow.templates", { dirs: this.templateDirs() });
    const list = Array.isArray(raw) ? raw : Array.isArray(raw?.templates) ? raw.templates : [];
    return list.map((t) => ({
      id: String(t.id ?? t.name ?? ""),
      name: String(t.name ?? t.id ?? ""),
      ...t.description ? { description: String(t.description) } : {},
      source: t.source === "user" || t.source === "project" ? t.source : "builtin",
      ...t.path ? { path: String(t.path) } : {},
      steps: Array.isArray(t.steps) ? t.steps.length : Number(t.steps ?? 0),
      inputs: (Array.isArray(t.inputs) ? t.inputs : []).map((i) => ({
        id: String(i.id ?? i.name ?? ""),
        label: String(i.label ?? i.id ?? i.name ?? ""),
        kind: String(i.kind ?? i.type ?? "text"),
        required: i.required === true,
        ...i.default !== void 0 && i.default !== null ? { default: String(i.default) } : {},
        ...Array.isArray(i.options) && i.options.length ? { options: i.options.map(String) } : {}
      })),
      ...t.error ? { error: String(t.error) } : {}
    }));
  }
  wireRequests() {
    const { projects, runs, needs, agents, host, ctx } = this;
    const handlers = {
      "overview.get": async () => {
        await projects.discover();
        return {
          projects: projects.metas().map((m) => projects.summary(m.root)).filter((p) => p !== null),
          runs: runs.metas().map((m) => runs.summary(m.dir)).filter((r) => r !== null),
          needs: needs.list(),
          agents: agents.list(),
          lang: ctx.lang()
        };
      },
      "project.get": ({ root }) => projects.view(root),
      "project.init": (p) => projects.init(p),
      "project.autopilot": ({ root, on }) => projects.setAutopilot(root, on),
      "project.task.create": (p) => projects.taskCreate(p),
      "project.task.update": (p) => projects.taskUpdate(p),
      "project.task.move": (p) => projects.taskMove(p),
      "project.notice": ({ root, text: text2 }) => projects.notice(root, text2),
      "team.get": ({ root }) => projects.teamGet(root),
      "team.save": ({ root, members }) => projects.teamSave(root, members),
      "role.start": async ({ root, role }) => {
        await projects.startRole(root, role);
        projects.changed(root);
      },
      "role.stop": ({ root, role }) => projects.stopRole(root, role),
      "agent.focus": ({ terminalId }) => agents.focus(terminalId),
      "agent.nudge": ({ terminalId }) => agents.nudge(terminalId),
      "templates.list": () => this.templates(),
      "template.validate": ({ text: text2 }) => {
        const raw = ctx.core.call("workflow.template.validate", { text: text2 });
        const errors = mapErrors(Array.isArray(raw) ? raw : raw?.errors);
        return { errors, ...Array.isArray(raw?.steps) ? { steps: raw.steps } : {} };
      },
      "run.start": (p) => runs.start(p),
      "run.get": ({ dir }) => runs.view(dir),
      "run.decide": (p) => runs.decide(p),
      "run.pause": ({ dir }) => runs.control(dir, "pause"),
      "run.resume": ({ dir }) => runs.control(dir, "resume"),
      "run.cancel": ({ dir }) => runs.control(dir, "cancel"),
      "run.retry": (p) => runs.retry(p),
      "need.resolve": ({ id, answer }) => needs.resolve(id, answer),
      "board.open": ({ root }) => this.openBoard(root),
      "run.open": ({ dir }) => this.openRun(dir),
      "view.expand": async ({ mode }) => {
        if (mode === "sidebar") {
          const p = ctx.store.data.projects[0];
          if (p) {
            try {
              await this.openBoard(p.root);
              return;
            } catch {
            }
          }
          const r = ctx.store.data.runs[0];
          if (r) {
            try {
              await this.openRun(r.dir);
              return;
            } catch {
            }
          }
        }
        for (const action of ["sidebar.left.toggle", "sidebar.right.toggle"]) {
          try {
            await ctx.host.call("commands.run", { action });
          } catch {
          }
        }
      },
      "link.read": ({ text: text2 }) => readLink(text2)
    };
    for (const name of Object.keys(handlers)) {
      const handler = handlers[name];
      this.disposables.push(
        host.handle(name, async (input) => {
          try {
            return await handler(input ?? {}) ?? null;
          } catch (error) {
            host.log(`${name}: ${errorText(error)}`);
            throw error;
          }
        })
      );
    }
  }
};

// worker/src/main.ts
var service = null;
function fsRoots() {
  if (process.platform !== "win32") return ["/"];
  const drives = [];
  for (let c = 65; c <= 90; c++) {
    const root = `${String.fromCharCode(c)}:\\`;
    if (existsSync6(root)) drives.push(root);
  }
  return drives;
}
async function activate(orca) {
  const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
  const host = orcaHost(orca);
  const core = await Core.load(join7(root, "core", "atrium_core.wasm"), fsRoots());
  const win = process.platform === "win32";
  const bin = join7(root, "bin");
  service = await Service.create(host, {
    core,
    binDir: bin,
    socket: socketPath(root),
    cli: win ? `"${join7(bin, "atrium.cmd")}"` : `node "${join7(bin, "atrium")}"`
  });
  host.log("atrium: ready");
}
async function deactivate() {
  const s = service;
  service = null;
  await s?.dispose();
}
export {
  deactivate,
  activate as default
};
