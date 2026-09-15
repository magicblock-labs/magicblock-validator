export interface StackArgs {
  enableVrf: boolean;
  enableErVrf: boolean;
  passthroughArgs: string[];
}

export function parseStackArgs(args: string[]): StackArgs {
  return {
    enableVrf: args.includes("--vrf"),
    enableErVrf: args.includes("--vrf-er"),
    passthroughArgs: args.filter(
      (arg) => arg !== "--vrf" && arg !== "--vrf-er",
    ),
  };
}

export function vrfOracleArgs(host: string, erPort: number): string[] {
  return [
    "--rpc-url",
    `http://${host}:${erPort}`,
    "--websocket-url",
    `ws://${host}:${erPort + 1}`,
  ];
}

export function baseVrfOracleArgs(
  host: string,
  basePort: number,
  remotesOverride?: string,
): string[] {
  if (remotesOverride) {
    const remotes = remotesOverride.split(",").map((remote) => remote.trim());
    const findUrl = (protocols: string[]) =>
      remotes.find((remote) => {
        try {
          const url = new URL(remote);
          return protocols.includes(url.protocol) && url.hostname.length > 0;
        } catch {
          return false;
        }
      });
    const rpcUrl = findUrl(["http:", "https:"]);
    const websocketUrl = findUrl(["ws:", "wss:"]);
    if (rpcUrl && websocketUrl) {
      return ["--rpc-url", rpcUrl, "--websocket-url", websocketUrl];
    }
    throw new Error(
      "--vrf with MB_STACK_ER_REMOTES requires explicit HTTP(S) and WS(S) URLs",
    );
  }
  return vrfOracleArgs(host, basePort);
}

export interface VrfServiceConfig {
  name: string;
  tag: string;
  args: string[];
}

export function vrfServiceConfigs(
  enableVrf: boolean,
  enableErVrf: boolean,
  host: string,
  basePort: number,
  erPort: number,
  remotesOverride?: string,
): VrfServiceConfig[] {
  const services: VrfServiceConfig[] = [];
  if (enableVrf) {
    services.push({
      name: "vrf-oracle (base)",
      tag: "vrf-base",
      args: baseVrfOracleArgs(host, basePort, remotesOverride),
    });
  }
  if (enableErVrf) {
    services.push({
      name: "vrf-oracle (ER)",
      tag: "vrf-er",
      args: vrfOracleArgs(host, erPort),
    });
  }
  return services;
}
