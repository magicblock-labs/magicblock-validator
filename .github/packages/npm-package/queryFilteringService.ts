#!/usr/bin/env node
import fs from "fs";
import { spawn, spawnSync } from "child_process";
import path from "path";
import { arch, platform } from "os";
import { VERSIONS } from "./getVersions";

const PACKAGE_VERSION = `query-filtering-service ${VERSIONS.QUERY_FILTERING_SERVICE}`;

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
  const binaryName = `@magicblock-labs/query-filtering-service-${os}-${arch()}/bin/query-filtering-service${extension}`;
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
      return;
    }
    process.exit(code ?? 1);
  });

  process.on("SIGINT", () => {
    child.kill("SIGINT");
    child.kill("SIGTERM");
  });
}

function runQueryFilteringService(location: string): Promise<boolean> {
  const args = process.argv.slice(2);
  const env = {
    ...process.env,
  };
  const queryFilteringService = spawn(location, args, {
    stdio: "inherit",
    env,
  });
  runWithForwardedExit(queryFilteringService);

  return new Promise((resolve) => {
    queryFilteringService.once("spawn", () => resolve(true));
    queryFilteringService.once("error", (error) => {
      console.error("Failed to spawn query-filtering-service:", error.message);
      resolve(false);
    });
  });
}

async function tryPackageQueryFilteringService(): Promise<boolean> {
  try {
    const resolvedPath = getExePath();
    return await runQueryFilteringService(resolvedPath);
  } catch (e) {
    console.error(
      "Failed to run query-filtering-service from package:",
      e instanceof Error ? e.message : e,
    );
    return false;
  }
}

function trySystemQueryFilteringService(): void {
  const absolutePath = process.env.PATH?.split(path.delimiter)
    .filter((dir) => dir !== path.dirname(process.argv[1]))
    .find((dir) => {
      try {
        fs.accessSync(`${dir}/query-filtering-service`, fs.constants.X_OK);
        return true;
      } catch {
        return false;
      }
    });

  if (!absolutePath) {
    console.error("Could not find globally installed query-filtering-service.");
    process.exit(1);
  }

  const absoluteBinaryPath = `${absolutePath}/query-filtering-service`;
  const [error, binaryVersion] = getBinaryVersion(absoluteBinaryPath);

  if (error !== null) {
    console.error(`Failed to get version of global binary: ${error}`);
    process.exit(1);
  }
  if (binaryVersion !== PACKAGE_VERSION) {
    console.error(
      `Globally installed query-filtering-service version is not correct. Expected "${PACKAGE_VERSION}", found "${binaryVersion}".`,
    );
    process.exit(1);
  }

  void runQueryFilteringService(absoluteBinaryPath);
}

async function main(): Promise<void> {
  if (!(await tryPackageQueryFilteringService())) {
    trySystemQueryFilteringService();
  }
}

void main();
