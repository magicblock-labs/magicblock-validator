#!/usr/bin/env node
//
// mb-stack: boot the full local MagicBlock stack as one supervised process.
//
//   client -> query-filtering-service -> ephemeral-validator -> mb-test-validator
//             http PUBLIC / ws PUBLIC+1   http ER / ws ER+1       http BASE / ws BASE+1
//             (public entry)              (internal)              (internal, base L1)
//
// It spawns the existing sibling wrapper scripts (mbTestValidator.js,
// ephemeralValidator.js, queryFilteringService.js). It only adds ordering, health
// gating, an endpoint summary, and supervision.
//
import http from "http";
import path from "path";
import { spawn, ChildProcess } from "child_process";

const HOST = "127.0.0.1";

function port(envVar: string, fallback: number): number {
  const raw = process.env[envVar];
  if (!raw) return fallback;
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed <= 0 || parsed > 65535) {
    console.error(`Invalid ${envVar}="${raw}", using default ${fallback}`);
    return fallback;
  }
  return parsed;
}

const PUBLIC_PORT = port("MB_STACK_PUBLIC_PORT", 6699);
const ER_PORT = port("MB_STACK_ER_PORT", 7799);
const BASE_PORT = port("MB_STACK_BASE_PORT", 8899);

// Which base chain the ephemeral-validator clones from / commits to. Defaults to
// the local mb-test-validator; override (comma-separated remotes, e.g.
// MB_STACK_ER_REMOTES=devnet) to point the ER at a real provisioned base.
function erRemotes(): string[] {
  const override = process.env.MB_STACK_ER_REMOTES;
  if (override) {
    return override
      .split(",")
      .map((r) => r.trim())
      .filter((r) => r.length > 0)
      .flatMap((r) => ["--remotes", r]);
  }
  return [
    "--remotes",
    `http://${HOST}:${BASE_PORT}`,
    "--remotes",
    `ws://${HOST}:${BASE_PORT + 1}`,
  ];
}

interface Service {
  name: string;
  script: string;
  args: string[];
  // RPC port to poll for readiness before starting the next service.
  healthPort: number;
  // Validator RPCs must report getHealth=ok before dependents can safely start.
  requireRpcHealth?: boolean;
  // Optional account that must be visible through RPC before dependents start.
  requiredAccount?: string;
  // Optional child-output marker that must appear before dependents start.
  readyOutput?: RegExp;
  // Short label + ANSI color used to prefix this service's log lines so the
  // three interleaved outputs stay readable.
  tag: string;
  color: string;
  extraEnv?: NodeJS.ProcessEnv;
}

const RESET = "\x1b[0m";
const useColor = process.stdout.isTTY === true;
const BOLD = "\x1b[1m";
const DIM = "\x1b[2m";
const GREEN = "\x1b[32m";
const CYAN = "\x1b[36m";
const MAGENTA = "\x1b[35m";

function color(text: string, ansi: string): string {
  return useColor ? `${ansi}${text}${RESET}` : text;
}

function svcColor(name: string): string {
  return SERVICES.find((svc) => svc.name === name)?.color ?? RESET;
}

// Extra CLI args passed to `mb-stack` are forwarded to the mb-test-validator
// (base L1), which appends them to solana-test-validator — the service operators
// most often need to customize (extra --bpf-program / --account / --clone / --reset).
// They are appended after mb-stack's own base args, so on last-wins flags the
// user's value takes priority. (Use MB_STACK_BASE_PORT instead of --rpc-port,
// which solana-test-validator rejects if given twice.)
const PASSTHROUGH_ARGS = process.argv.slice(2);

// Ordered: base L1 first, then ER (which clones from base), then the public
// query-filtering-service front (which forwards to the ER).
const SERVICES: Service[] = [
  {
    name: "mb-test-validator (base L1)",
    script: "mbTestValidator.js",
    args: ["--rpc-port", String(BASE_PORT), ...PASSTHROUGH_ARGS],
    healthPort: BASE_PORT,
    requiredAccount: "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh",
    readyOutput: /^JSON RPC URL:/,
    tag: "base",
    color: "\x1b[36m", // cyan
  },
  {
    name: "ephemeral-validator",
    script: "ephemeralValidator.js",
    // --no-tui is required: the published ER is built with the TUI feature, and a
    // supervised child with redirected stdio (no TTY) would otherwise exit
    // immediately. The stack is headless by design.
    args: ["--no-tui", "--listen", `${HOST}:${ER_PORT}`, ...erRemotes()],
    healthPort: ER_PORT,
    tag: "er",
    color: "\x1b[32m", // green
  },
  {
    name: "query-filtering-service",
    script: "queryFilteringService.js",
    args: [
      "--listen-addr",
      `${HOST}:${PUBLIC_PORT}`,
      "--listen-addr-ws",
      `${HOST}:${PUBLIC_PORT + 1}`,
      "--ephemeral-url",
      `http://${HOST}:${ER_PORT}`,
      "--ephemeral-url-ws",
      `ws://${HOST}:${ER_PORT + 1}`,
    ],
    healthPort: PUBLIC_PORT,
    requireRpcHealth: false,
    tag: "qfs",
    color: "\x1b[35m", // magenta
  },
];

const readyOutputSeen = new Set<string>();
const ERROR_LINE_RE = /\b(error|failed|fatal|panic|panicked)\b/i;

// Monitor child output without echoing normal logs. Readiness markers still need
// to be observed, and error lines are surfaced with a service prefix.
function monitorOutput(stream: NodeJS.ReadableStream, svc: Service): void {
  const prefix = useColor
    ? `${svc.color}${svc.tag}${RESET} | `
    : `${svc.tag} | `;
  const handleLine = (line: string) => {
    if (svc.readyOutput?.test(line)) readyOutputSeen.add(svc.name);
    if (ERROR_LINE_RE.test(line)) {
      process.stderr.write(prefix + line + "\n");
    }
  };

  let buf = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk: string) => {
    buf += chunk;
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      handleLine(buf.slice(0, nl));
      buf = buf.slice(nl + 1);
    }
  });
  stream.on("end", () => {
    if (buf.length > 0) {
      handleLine(buf);
    }
  });
}

const children: { name: string; child: ChildProcess }[] = [];
let shuttingDown = false;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function postRpc(
  healthPort: number,
  method: string,
  params: unknown[] = [],
): Promise<unknown | null> {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (result: unknown | null) => {
      if (settled) return;
      settled = true;
      resolve(result);
    };

    const body = JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method,
      params,
    });
    const req = http.request(
      {
        host: HOST,
        port: healthPort,
        path: "/",
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Content-Length": Buffer.byteLength(body),
        },
        timeout: 2000,
      },
      (res) => {
        let responseBody = "";
        res.setEncoding("utf8");
        res.on("data", (chunk: string) => {
          responseBody += chunk;
        });
        res.on("end", () => {
          try {
            const payload = JSON.parse(responseBody) as { result?: unknown };
            finish(payload.result ?? null);
          } catch {
            finish(null);
          }
        });
      },
    );
    req.on("error", () => finish(null));
    req.on("timeout", () => {
      req.destroy();
      finish(null);
    });
    req.write(body);
    req.end();
  });
}

function pingPort(healthPort: number): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.request(
      {
        host: HOST,
        port: healthPort,
        path: "/",
        method: "POST",
        timeout: 2000,
      },
      (res) => {
        res.resume();
        resolve(true);
      },
    );
    req.on("error", () => resolve(false));
    req.on("timeout", () => {
      req.destroy();
      resolve(false);
    });
    req.end();
  });
}

// Poll JSON-RPC getHealth until validator RPCs report `ok`. The base
// validator also waits for solana-test-validator's own readiness line because
// Agave can answer health checks before that startup gate has completed.
async function pingHealth(
  healthPort: number,
  requireRpcHealth: boolean,
  requiredAccount?: string,
): Promise<boolean> {
  if (!requireRpcHealth) return pingPort(healthPort);

  const health = await postRpc(healthPort, "getHealth");
  if (health !== "ok") return false;

  if (!requiredAccount) return true;
  const account = await postRpc(healthPort, "getAccountInfo", [
    requiredAccount,
    { encoding: "base64" },
  ]);
  return (
    typeof account === "object" &&
    account !== null &&
    "value" in account &&
    account.value !== null
  );
}

async function waitForHealth(
  name: string,
  healthPort: number,
  requireRpcHealth: boolean,
  requiredAccount?: string,
  waitForReadyOutput = false,
  timeoutMs = 120000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (shuttingDown) return;
    const outputReady = !waitForReadyOutput || readyOutputSeen.has(name);
    if (
      outputReady &&
      (await pingHealth(healthPort, requireRpcHealth, requiredAccount))
    ) {
      console.log(
        `${color("✔", GREEN)} ${color(name, svcColor(name))} ready: ${color(
          "rpc",
          DIM,
        )} http://${HOST}:${healthPort}, ${color("ws", DIM)} ws://${HOST}:${
          healthPort + 1
        }`,
      );
      return;
    }
    await sleep(500);
  }
  throw new Error(
    `${name} did not become healthy on ${HOST}:${healthPort} within ${timeoutMs}ms`,
  );
}

// Each service is a wrapper script (node) that in turn spawns the real binary
// (a grandchild). Killing only the direct child would orphan that grandchild, so
// every service is spawned `detached` as its own process-group leader and we
// signal the whole group (negative pid) to bring the entire subtree down.
function killGroup(child: ChildProcess, signal: NodeJS.Signals): void {
  if (child.pid === undefined) return;
  if (child.exitCode !== null || child.signalCode !== null) return;
  try {
    process.kill(-child.pid, signal);
  } catch {
    // Group already gone (ESRCH) — nothing to do.
  }
}

function shutdown(code: number): void {
  if (shuttingDown) return;
  shuttingDown = true;
  for (const { child } of children) {
    killGroup(child, "SIGTERM");
  }
  // Give children a moment to exit cleanly, then force exit.
  setTimeout(() => process.exit(code), 5000).unref();
}

function startService(svc: Service): ChildProcess {
  const scriptPath = path.join(__dirname, svc.script);
  const child = spawn(process.execPath, [scriptPath, ...svc.args], {
    // Capture stdout/stderr so readiness markers can be observed and error
    // lines can be surfaced without forwarding noisy subprocess logs.
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
    env: { ...process.env, ...svc.extraEnv },
  });
  if (child.stdout) monitorOutput(child.stdout, svc);
  if (child.stderr) monitorOutput(child.stderr, svc);
  children.push({ name: svc.name, child });

  child.on("exit", (code, signal) => {
    if (shuttingDown) return;
    console.error(
      `\n✖ ${svc.name} exited unexpectedly (code=${code}, signal=${signal}). Shutting down the stack.`,
    );
    shutdown(code ?? 1);
  });
  child.on("error", (err) => {
    if (shuttingDown) return;
    console.error(`\n✖ Failed to start ${svc.name}: ${err.message}`);
    shutdown(1);
  });

  return child;
}

function printSummary(): void {
  console.log("");
  console.log(color("MagicBlock stack is ready.", `${BOLD}`));
  const remotesOverride = process.env.MB_STACK_ER_REMOTES;
  if (remotesOverride) {
    console.log(`${color("Base chain:", CYAN)}             ${remotesOverride}`);
  }
  console.log(color("Stop: press Ctrl-C to shut down all nodes.", DIM));
  console.log("");
}

async function main(): Promise<void> {
  process.on("SIGINT", () => shutdown(0));
  process.on("SIGTERM", () => shutdown(0));

  // When the ER points at an external base, the local base validator is unused —
  // skip it so we don't boot an idle validator.
  const services = process.env.MB_STACK_ER_REMOTES
    ? SERVICES.filter((s) => s.script !== "mbTestValidator.js")
    : SERVICES;

  for (const svc of services) {
    startService(svc);
    await waitForHealth(
      svc.name,
      svc.healthPort,
      svc.requireRpcHealth ?? true,
      svc.requiredAccount,
      svc.readyOutput !== undefined,
    );
    if (shuttingDown) return;
  }

  printSummary();
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  shutdown(1);
});
