export interface StackArgs {
  enableVrf: boolean;
  passthroughArgs: string[];
}

export function parseStackArgs(args: string[]): StackArgs {
  return {
    enableVrf: args.includes("--vrf"),
    passthroughArgs: args.filter((arg) => arg !== "--vrf"),
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
