#!/usr/bin/env node
import fs from "fs";
import { spawn, spawnSync } from "child_process";
import path from "path";
import { arch, platform } from "os";
import { VERSIONS } from "./getVersions";

const PACKAGE_VERSION = `hydra-cranker ${VERSIONS.HYDRA_CRANKER}`;

function getBinaryVersion(location: string): [string | null, string | null] {
  const result = spawnSync(location, ["--version"]);
  const error: string | null =
    (result.error && result.error.toString()) ||
    (result.stderr.length > 0 && result.stderr.toString().trim()) ||
    null;
  return [error, result.stdout && result.stdout.toString().trim()];
}

function getExePath(): string {
  let os: string = platform();
  let extension = "";
  if (["win32", "cygwin"].includes(os)) {
    os = "windows";
    extension = ".exe";
  }
  const binaryName = `@magicblock-labs/hydra-cranker-${os}-${arch()}/bin/hydra-cranker${extension}`;
  try {
    return require.resolve(binaryName);
  } catch (e) {
    throw new Error(
      `Couldn't find application binary inside node_modules for ${os}-${arch()}, expected location: ${binaryName}`,
    );
  }
}

function runWithForwardedExit(child: ReturnType<typeof spawn>): void {
  child.on("exit", (code: number | null, signal: NodeJS.Signals | null) => {
    if (signal) {
      process.kill(process.pid, signal);
    } else {
      process.exit(code ?? 1);
    }
  });

  process.on("SIGINT", () => {
    child.kill("SIGINT");
    child.kill("SIGTERM");
  });
}

function runHydraCranker(location: string): void {
  const args = process.argv.slice(2);
  const env = {
    ...process.env,
  };
  const hydraCranker = spawn(location, args, { stdio: "inherit", env });
  runWithForwardedExit(hydraCranker);
}

function tryPackageHydraCranker(): boolean {
  try {
    const path = getExePath();
    runHydraCranker(path);
    return true;
  } catch (e) {
    console.error(
      "Failed to run hydra-cranker from package:",
      e instanceof Error ? e.message : e,
    );
    return false;
  }
}

function trySystemHydraCranker(): void {
  const absolutePath = process.env.PATH?.split(path.delimiter)
    .filter((dir) => dir !== path.dirname(process.argv[1]))
    .find((dir) => {
      try {
        fs.accessSync(`${dir}/hydra-cranker`, fs.constants.X_OK);
        return true;
      } catch {
        return false;
      }
    });

  if (!absolutePath) {
    console.error(
      `Could not find globally installed hydra-cranker, please install with cargo.`,
    );
    process.exit(1);
  }

  const absoluteBinaryPath = `${absolutePath}/hydra-cranker`;
  const [error, binaryVersion] = getBinaryVersion(absoluteBinaryPath);

  if (error !== null) {
    console.error(`Failed to get version of global binary: ${error}`);
    process.exit(1);
  }
  if (binaryVersion !== PACKAGE_VERSION) {
    console.error(
      `Globally installed hydra-cranker version is not correct. Expected "${PACKAGE_VERSION}", found "${binaryVersion}".`,
    );
    process.exit(1);
  }

  runHydraCranker(absoluteBinaryPath);
}

// Try to run hydra-cranker from package first, then fall back to system installation.
tryPackageHydraCranker() || trySystemHydraCranker();
