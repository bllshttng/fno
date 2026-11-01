# Mux feel: what a keypress costs

`crates/fno/examples/mux_latency.rs` plays the host terminal. It writes one key and times the echo at the host pty. Ghostty paints on both sides of the comparison, so the difference between scenarios is what the mux adds.

mux_added_p95_ms: 5.17
flood_added_p95_ms: 10.17
flood_over_alone_p95_ms: 3.61
flood_verdict: fixed
pixel_leg: unmeasured
verdict: stop

`mux_added_p95_ms` is scenario C minus scenario A on the deployed build at a load near the bar. `flood_added_p95_ms` is scenario D minus scenario A on the fix. `flood_over_alone_p95_ms` is scenario D minus scenario C on the fix, the higher of two full runs. The target is one 60 Hz frame, 16.7 ms.

## A flooding sibling: the cause and the fix

The client writer took the first dirty pane in hash order. A flooding pane is dirty again by the time its frame is on the socket. So on an unlucky hash seed the flood kept the socket, and the focused pane's echo waited until the flood paused. The hash seed changes per attach, so the same build read 10 ms on one attach and lost samples on the next.

The writer now rotates through dirty panes in id order (`server/client_writer.rs`). A dirty pane waits at most one write per other dirty pane.

### The trace

A throwaway build timed each stage at scenario D, in three rounds at 5 minute load 15 to 16. Times are p95 in milliseconds. Only the focused pane's dirty wait tracks the cost.

| Stage | Round 1 | Round 2 | Round 3 |
|---|---|---|---|
| D minus C, end to end | 73.53 | 774.66 | 950.77 |
| Focused pane, dirty insert to write | 14.32 | 508.11 | 635.18 |
| Flooding pane, dirty insert to write | 3.73 | 1.53 | 1.41 |
| Socket write | 3.83 | 1.68 | 1.49 |
| Core drain pass | 0.14 | 0.08 | 0.07 |
| Key waiting for the core | 1.67 | 1.91 | 0.79 |
| Client compose and draw | 0.68 | 0.57 | 0.46 |
| Client frame queue depth | 0 | 0 | 0 |

In round 3 the writer sent 141,578 frames, and the focused pane got 1,011 of them. Its longest wait was 2.45 s.

Scenario E agrees. It floods in a tab nobody views, so the flood builds no frames. E minus C read 4.84 and 2.79 ms at p95, while D minus C read 27.12 and 689.19 ms in the same two runs. Those runs ended at 5 minute load 14.2 and 15.0, over the bar of 12, so they are loaded readings.

### Paired runs

These alternate the deployed build and the fix, at 500 samples. Each round starts with 5 minute load at or under 12. A round is a new attach, so it draws a new hash seed. Times are D minus C in milliseconds.

| Round | Deployed p95 | Fix p95 |
|---|---|---|
| 1 | 5.92 | 3.50 |
| 2 | 0.35 | 2.51 |
| 3 | void: 35 of 500 samples lost | 0.64 |
| 4 | void: 28 of 500 samples lost | 0.87 |

A lost sample is an echo that took over 2 s. The harness voids a run that loses over 1 percent. The deployed build's good rounds read as low as the fix, because a lucky seed never starved. The fix removes the unlucky seeds, and it lost no sample in any round.

The fix did not raise scenario C. At p95 the deployed build read 4.44, 3.94, 7.44 and 3.80 ms. The fix read 3.28, 7.79, 3.34 and 3.73 ms.

Two full runs of the fix at 2,000 samples:

| Run | C p95 | D p95 | D p99 | D minus C p95 | Lost |
|---|---|---|---|---|---|
| 1 | 6.69 | 10.31 | 29.89 | 3.61 | 0 |
| 2 | 9.50 | 4.38 | 9.21 | -5.12 | 0 |

C reads higher in these runs than in the paired rounds. They are not paired, and one pane has only one dirty key, so the rotation does not change its path. The negative number is noise in C.

### What this does not cover

`yes` is the worst case. This work does not prove that a real agent stream starved a pane, because agent output is bursty and far slower. Scheduler delay under fleet load is also outside this fix.

## Read this next: the older numbers came from a busy machine

The tables below predate the fix. The 5 minute load average stayed between 27 and 111 on 12 CPUs. The 10 ms control held in every full run: it read 9.9 to 10.9 ms above bare. So the clock is sound, but p95 and p99 carry scheduler delay from other work. C reads 4 to 6 ms at p95 near the quiet bar, not 18.68.

## Numbers

The table uses 2,000 samples per scenario with the deployed release `fno` 0.4.5, and panes run through a keeper, as a user's pane does. Times are in milliseconds.

| Scenario | p50 | p95 | p99 |
|---|---|---|---|
| A, bare pty, `cat` | 0.03 | 0.07 | 0.31 |
| Control, A plus a 10 ms delay | 10.04 | 10.15 | 11.17 |
| B, mux, 80x24 | 0.90 | 6.27 | 16.53 |
| C, mux, 200x50 | 3.70 | 18.75 | 41.51 |
| D, as C plus a `yes` sibling pane | 24.02 | 219.62 | 781.75 |

A second run of the same build read 17.71 p95 for C and 601.70 p95 for D, at higher load. D lost 8 samples. The flood's cost is large in every run, and its size moves with the load.

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

Thresholds rest on display frame times: 8.3 ms at 120 Hz and 16.7 ms at 60 Hz. On the busy machine, scenario C read 18.68 ms against 8.3, and D read 219.55 ms against 16.7.

The wire stages total 3.06 ms at p95. That is 16 percent of C's busy p95, and the rule for grid diffs needs at least half. So grid diffs do not run, and neither does the libghostty-vt spike. Near the quiet bar the share is higher: 59 percent of 5.17 ms. The verdict holds for a different reason, because 5.17 ms is under the 8.3 ms threshold. Replacing the VT does not help either: `feed` and `frame` total 0.07 ms.

At p50 the picture is closer. C adds 3.67 ms, and the wire stages are 1.75 ms of that. A diff can save up to about 1.7 ms on a typical key, which is under one display frame.

Moving frame bytes is not what a user feels here.

## Known noise in the harness

The isolated server also runs its Python helper from the scratch home. One CPU sample caught it at 66 percent of a core. A user's server runs the same helper, but its timing can differ.

## Repeat the run

```bash
cargo build --release --manifest-path crates/fno/Cargo.toml --example mux_latency
crates/fno/target/release/examples/mux_latency --fno <release fno>
crates/fno/target/release/examples/mux_latency --fno <release fno> --scenario C,D,E
crates/fno/target/release/examples/mux_latency --stages
fno doctor footprint
```

A full run takes 10 to 15 minutes. `--scenario` picks from B, C, D and E, and A and the control always run. Compare a flood across several attaches, because each attach draws a new hash seed. The harness uses its own mux directory and home, so it never touches the live fleet. When the control misses by more than 2 ms, it exits 2 and prints no mux numbers.
