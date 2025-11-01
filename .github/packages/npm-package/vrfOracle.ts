#!/usr/bin/env node
import fs from "fs";
import { spawn, spawnSync } from "child_process";
import path from "path";
import { arch, platform } from "os";
import { VERSIONS } from "./versions";

const PACKAGE_VERSION = `vrf-oracle ${VERSIONS.VRF_ORACLE}`;

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
  const binaryName = `@magicblock-labs/vrf-oracle-${os}-${arch()}/bin/vrf-oracle${extension}`;
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

function runVrfOracle(location: string): void {
  const args = process.argv.slice(2);
  const env = {
    ...process.env,
    RUST_LOG: "quiet",
  };
  const vrfOracle = spawn(location, args, { stdio: "inherit", env});
  runWithForwardedExit(vrfOracle);
}

function tryPackageVrfOracle(): boolean {
  try {
    const path = getExePath();
    runVrfOracle(path);
    return true;
  } catch (e) {
    console.error(
      "Failed to run vrf-oracle from package:",
      e instanceof Error ? e.message : e,
    );
    return false;
  }
}

function trySystemVrfOracle(): void {
  const absolutePath = process.env.PATH?.split(path.delimiter)
    .filter((dir) => dir !== path.dirname(process.argv[1]))
    .find((dir) => {
      try {
        fs.accessSync(`${dir}/vrf-oracle`, fs.constants.X_OK);
        return true;
      } catch {
        return false;
      }
    });

  if (!absolutePath) {
    console.error(
      `Could not find globally installed vrf-oracle, please install with cargo.`,
    );
    process.exit(1);
  }

  const absoluteBinaryPath = `${absolutePath}/vrf-oracle`;
  const [error, binaryVersion] = getBinaryVersion(absoluteBinaryPath);

  if (error !== null) {
    console.error(`Failed to get version of global binary: ${error}`);
    return;
  }
  if (binaryVersion !== PACKAGE_VERSION) {
    console.error(
      `Globally installed vrf-oracle version is not correct. Expected "${PACKAGE_VERSION}", found "${binaryVersion}".`,
    );
    return;
  }

  runVrfOracle(absoluteBinaryPath);
}

// If the first argument is our special command, run the test validator and exit.
tryPackageVrfOracle() || trySystemVrfOracle();
