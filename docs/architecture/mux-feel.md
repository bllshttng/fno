# Mux feel: what a keypress costs

`crates/fno/examples/mux_latency.rs` plays the host terminal. It writes one key and times the echo at the host pty. Ghostty paints on both sides of the comparison, so the difference between scenarios is what the mux adds.

mux_added_p95_ms: 18.68
flood_added_p95_ms: 219.55
pixel_leg: unmeasured
verdict: stop

`mux_added_p95_ms` is scenario C minus scenario A. `flood_added_p95_ms` is scenario D minus scenario A.

## Read this first: the machine was busy

These numbers come from a machine that never went calm. The 5 minute load average stayed between 27 and 111 on 12 CPUs, during an active fleet, and the lead ordered the numbers after the incident cleared. The 10 ms control held in every full run: it read 9.9 to 10.9 ms above bare. So the clock is sound, but p95 and p99 carry scheduler delay from other work. Treat the p50 column as the steadier read. Run the harness again on a quiet machine before you trust a p95 to the millisecond.

## Numbers

All runs use 2,000 samples per scenario with the deployed release `fno` 0.4.5, and panes run through a keeper, as a user's pane does. Times are in milliseconds.

| Scenario | p50 | p95 | p99 |
|---|---|---|---|
| A, bare pty, `cat` | 0.03 | 0.07 | 0.31 |
| Control, A plus a 10 ms delay | 10.04 | 10.15 | 11.17 |
| B, mux, 80x24 | 0.90 | 6.27 | 16.53 |
| C, mux, 200x50 | 3.70 | 18.75 | 41.51 |
| D, as C plus a `yes` sibling pane | 24.02 | 219.62 | 781.75 |

A second run of the same build, at higher load, read 17.71 p95 for C and 601.70 p95 for D, with 8 lost samples in D. The flood's cost is large in every run, and its size moves with the load.

Footprint at the start of the table's run: fleet CPU 0.533 cores, load 40.1 (5 minutes). At the end: fleet CPU 4.823 cores, load 50.0.

The 200x50 scenarios are sized at the host terminal, and the sideline can take columns from the pane. D's split also halves the pane's width.

## Stage costs

`mux_latency --stages` times one frame on a real `Pane` at 200x50. A frame is 500,160 bytes on the wire.

| Stage | p50 | p95 |
|---|---|---|
| feed one key | 0.000 | 0.003 |
| `frame` | 0.037 | 0.067 |
| `clone` | 0.003 | 0.007 |
| encode | 0.613 | 1.126 |
| decode | 1.099 | 1.862 |
| total | 1.752 | 3.062 |

Compose and the client's row diff are `pub(super)`, so the example cannot time them. They stay inside the end-to-end number.

## The verdict

Thresholds rest on display frame times: 8.3 ms at 120 Hz and 16.7 ms at 60 Hz. Scenario C reads 18.68 ms against 8.3, and D reads 219.55 ms against 16.7. Both are over.

The wire stages total 3.06 ms at p95. That is 16 percent of C's added p95, and the rule for grid diffs needs at least half. So grid diffs do not run, and neither does the libghostty-vt spike. Replacing the VT does not help either: `feed` and `frame` total 0.07 ms.

At p50 the picture is closer. C adds 3.67 ms, and the wire stages are 1.75 ms of that. A diff can save up to about 1.7 ms on a typical key, which is under one display frame.

Moving frame bytes is not what a user feels here. The cost is the wait behind a sibling pane that floods.

## The largest stage: a flooding sibling

D's p50 is 6.5 times C's, and its tail reaches past a second. We have not found the cause. What we measured:

- Pane-output queue depth is not the cause. The shared queue in `server/serve.rs` holds 256 chunks, and a full queue takes about 330 ms to feed, which looked like the answer. We built with 16 chunks and measured: D read 752.99 ms at p95 against 601.70 ms for the deployed build in the next run. It did not help, so we reverted it.
- No single process is pegged during D. We sampled CPU every 5 seconds at load 87, in percent of one core:
  - server: 37 to 84
  - client: 36 to 70
  - the flooding pane's keeper: 25 to 62
  - `yes`: 6 to 17

Two things remain untested. Nothing paces frames: each drain of the output channel builds a frame, and `broadcast_pane` has no timer. The client also queues up to 16 whole frames at `client.rs` and composes and draws each one in turn.

## Repeat the run

```bash
cargo build --release --manifest-path crates/fno/Cargo.toml --example mux_latency
FNO_BIN=<release fno> crates/fno/target/release/examples/mux_latency
crates/fno/target/release/examples/mux_latency --stages
fno doctor footprint
```

A run takes 10 to 15 minutes. The harness uses its own mux directory and home, so it never touches the live fleet. When the control misses by more than 2 ms, it exits 2 and prints no mux numbers.
