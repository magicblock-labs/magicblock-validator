# magicblock-validator-admin

Claims accumulated validator fees from the base chain on behalf of the leader.

Enable periodic claims through `[admin]` in the
[leader configuration](../config.example.toml). Claims use the leader's local
signing authority; a verifier cannot claim its upstream's fees. Small balances
are skipped, and a failed claim is logged without stopping future attempts.

Domain registration is a separate operator action. Use the
[magicblock CLI](../bins/magicblock/README.md) to manage domain records.

[Back to workspace](../README.md)
