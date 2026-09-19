# magicblock-aml

Checks account addresses against the risk service before [Chainlink](../magicblock-chainlink/README.md)
runs post-delegation actions. The server manages risk thresholds and provider
credentials; this crate consumes its verdicts.

## Using it

Create a client from the validator's risk configuration. Disabled checks do not
create a client; enabled checks report flagged addresses and request failures
separately. A failed request is not a safe verdict.

Remote endpoints require HTTPS; HTTP is supported only on loopback addresses.
Passing a risk check does not grant permission to modify an account.

See the [leader configuration](../config.validator.example.toml) for settings.

[Back to workspace](../README.md)
