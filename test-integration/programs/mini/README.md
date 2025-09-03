## Mini Program

This Solana program exists in order to test program cloning into our validator.

It operates on a simple PDA counter account `Counter { count: u64 }` that can be incremented.

It has two instructions:

- Init - inits the counter
- Increment - adds one to the counter

It is written in pure Rust (no anchor).
