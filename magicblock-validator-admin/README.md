# `magicblock-validator-admin`

Periodic administrative work for the leader. Currently this crate claims
accumulated validator fees from the base-chain Delegation Program.

## Fee claims

`run_claim_fees_loop` waits for the configured period before the first claim,
then repeats until its shutdown handle is signalled. Individual claim errors
are logged and do not stop subsequent ticks.

`claim_fees` requires Engine's represented authority to match its local signer.
A follower identity cannot claim a remote authority's vault. Balances at or below
the minimum claim threshold are skipped; eligible claims are sent using the
local signer and a confirmed-commitment RPC client.

The leader enables this loop through its optional `[admin]` configuration.
Domain registration is not part of this crate: use the [operator CLI][operator].

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[operator]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/bins/magicblock/README.md
