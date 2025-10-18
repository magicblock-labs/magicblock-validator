#!/usr/bin/env node
import { spawn } from "child_process";
import * as path from "path";
import * as fs from "fs";

function runWithForwardedExit(child: ReturnType<typeof spawn>): void {
  child.on("exit", (code: number | null, signal: NodeJS.Signals | null) => {
    process.on("exit", () => {
      if (signal) {
        process.kill(process.pid, signal);
      } else if (code !== null) {
        process.exit(code);
      }
    });
  });

  process.on("SIGINT", () => {
    child.kill("SIGINT");
    child.kill("SIGTERM");
  });
}

function dumpsDir(): string {
  // Compiled js lives in lib/, source in package root. We want <package-root>/bin/local-dumps
  const libDir = __dirname;
  const root = path.resolve(libDir, "..");
  return path.join(root, "ephemeral-validator", "bin", "local-dumps");
}

function runMbTestValidator(): void {
  const exe = "solana-test-validator";
  const dumps = dumpsDir();
  const p = (name: string) => path.join(dumps, name);

  const args = [
    // programs
    "--bpf-program",
    "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh",
    p("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh.so"),
    "--bpf-program",
    "noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV",
    p("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV.so"),
    "--bpf-program",
    "Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz",
    p("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz.so"),
    "--bpf-program",
    "noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV",
    p("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV.so"),
    // accounts
    "--account",
    "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
    p("mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev.json"),
    "--account",
    "EpJnX7ueXk7fKojBymqmVuCuwyhDQsYcLVL1XMsBbvDX",
    p("EpJnX7ueXk7fKojBymqmVuCuwyhDQsYcLVL1XMsBbvDX.json"),
    "--account",
    "7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg",
    p("7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg.json"),
    "--account",
    "Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh",
    p("Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh.json"),
    "--account",
    "5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc",
    p("5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc.json"),
    "--account",
    "F72HqCR8nwYsVyeVd38pgKkjXmXFzVAM8rjZZsXWbdE",
    p("F72HqCR8nwYsVyeVd38pgKkjXmXFzVAM8rjZZsXWbdE.json"),
  ];

  const expectedFiles = [
    "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh.so",
    "noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV.so",
    "Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz.so",
    "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev.json",
    "EpJnX7ueXk7fKojBymqmVuCuwyhDQsYcLVL1XMsBbvDX.json",
    "7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg.json",
    "Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh.json",
    "5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc.json",
    "F72HqCR8nwYsVyeVd38pgKkjXmXFzVAM8rjZZsXWbdE.json",
  ];
  const missingFiles = expectedFiles
    .map((f) => p(f))
    .filter((full) => !fs.existsSync(full));
  if (missingFiles.length > 0) {
    console.warn("Warning: missing local dumps files:\n" + missingFiles.join("\n"));
  }

  const extraArgs = process.argv.slice(2);
  const child = spawn(exe, [...args, ...extraArgs], { stdio: "inherit" });
  runWithForwardedExit(child);
}

runMbTestValidator();
