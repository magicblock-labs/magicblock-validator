# `mbv-leader`

`bins/mbv-leader` is the production leader binary and the sole owner of the
leader service lifecycle.

## Responsibilities

- load `magicblock_config::LeaderParams`;
- construct the shared Keeper image through
  `magicblock_runtime::keeper_builder`;
- open `Engine<Leader>`;
- wire Chainlink, Aperture, settlement, scheduling, metrics, and compatibility
  ledger services;
- hold the deprecated-ledger lock for the process lifetime;
- treat unexpected engine service exit as fatal and stop services in order.

Magic Domain Program registration is not a leader lifecycle responsibility.
Do not add automatic registration, synchronization, or unregistration here;
those operations belong to `mbv`.

Keep `main.rs` process-oriented and keep the service graph in `leader.rs`.
Do not introduce an embedded TUI; `mbv-tui` is an external client.

The metrics endpoint exposes both the namespaced MBV collectors and Engine
collectors registered in the process-wide default Prometheus registry.

Focused validation:

```bash
cargo check -p mbv-leader --locked
```
