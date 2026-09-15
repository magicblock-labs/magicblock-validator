# `magicblock-aml`

Client for the risk server used by Chainlink when assessing post-delegation
action signers. The server owns upstream provider credentials, caching, and
risk thresholds; this crate consumes its verdict, not the raw provider score.

## Using the client

`RiskService::try_from_config` returns no service when checks are disabled.
When enabled, it validates the endpoint and builds a client with the configured
request timeout. `check_strategy` tells Chainlink which signers to select;
`check_addresses` queries `GET /risk?pubkey=...` for each supplied address.

Checks run concurrently across the supplied batch. A flagged address produces
`RiskError::HighRiskAddresses`; configuration, transport, and response-decoding
failures remain errors rather than safe verdicts.

## Trust boundary

Remote endpoints require HTTPS; HTTP is accepted only for loopback hosts.
Redirects are disabled. The client uses the server's `isRisky` response and
does not recompute its threshold. A risk verdict is not delegation evidence or
permission to mutate an account.

See [Chainlink][chainlink] for how verdicts affect activation.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[chainlink]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-chainlink/README.md
