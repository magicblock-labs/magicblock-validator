import fs from "fs";
import path from "path";

interface PackageJson {
  version: string;
  optionalDependencies: Record<string, string>;
}

function readPackageJson(): PackageJson {
  // When compiled to lib/, we need to go up one directory to find package.json
  const packageJsonPath = path.join(__dirname, "../package.json");
  const packageJsonContent = fs.readFileSync(packageJsonPath, "utf8");
  return JSON.parse(packageJsonContent);
}

export function getVersions() {
  const pkg = readPackageJson();

  return {
    EPHEMERAL_VALIDATOR: pkg.version,
    VRF_ORACLE: pkg.optionalDependencies["@magicblock-labs/vrf-oracle-linux-x64"],
    RPC_ROUTER: pkg.optionalDependencies["@magicblock-labs/rpc-router-linux-x64"],
  } as const;
}

export const VERSIONS = getVersions();