## Memory Issue

At this point the memory usage of our validator is increasing steadily. I've seen it go above
6GB. This could be due to many issues which I partially evaluated.

I'm fairly sure that this affects the performance of the validator as well as memory pressure
increases, even though this could be happening for many other reasons as the validator is
running for a long time and client apps get put into the background by the OS.

### Single Bank

We keep a single bank running which means that we don't do the same cleanup that is performed
whenever a new bank is created from a parent bank.

Thus we keep the transaction statuses around longer (and am cleaning them up roughly every 1min
to exclude that being the reason for the increased memory).

Additionally we never flush the accounts db to disk and I haven't found an obvious way to do so
without freezing the bank and creating a new one.

### Geyser Events Cache

In order to avoid transaction/account updates being missed when a subscription comes in late
we're caching them in the Geyser plugin. Items are evicted after a timeout.

I ruled out that this is the main reason for the memory increase as I observe it as well
without adding to that cache.
