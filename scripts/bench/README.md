# bench

A load benchmark for the yagura daemon (`bench.py`). It measures two builds under the same load,
interleaved in one run, and judges whether the second is worse than the first. It is a developer
tool: CI never calls it, and it is not a test.

## 1. Platform and prerequisites

- **Windows only.** Process statistics come from Win32 calls through `ctypes`
  (`CreateToolhelp32Snapshot`, `GetProcessTimes`, `GetProcessMemoryInfo`) and the client speaks to
  the daemon's named pipe. On macOS and Linux `run` prints a message and exits with code 1; the
  Unix-socket endpoint and `/proc` / `ps` statistics are not written. `compare` runs anywhere.
- `python` on `PATH` (measured with 3.11.9). **Standard library only**; no dependency is added.
- A release build of the daemon, the holder it starts for each window from beside itself, and the
  `hex_echo` fixture. `run` places the bundled ConPTY of this working tree
  (`vendor/conpty/x64/`) beside the staged copy of each build and checks it against the pins in
  this working tree's `crates/yagura-os/src/bundled_conpty_pins.rs` (read through
  `scripts/dep-audit/dep_audit.py`); a mismatch stops the run. A base build whose daemon carries
  other pins than the working tree gets the working tree's pair, which that daemon refuses, so
  such a build cannot be measured:

  ```bash
  CARGO_TARGET_DIR=D:/yagura-build/target-t1051 cargo build --release -p yagura-daemon -p yagura-holder --bins
  ```

## 2. What it does

`run` takes one `--bin-dir` per build (normally two: the base and the new one). It first copies the
top-level files of each into `<out stem>-work/bin-<i>/`, places the ConPTY pair there and runs
every build from there (see §4).
Each build gets its own instance, `<--instance>-<i>` (default `bench-t1051-0`, `bench-t1051-1`), so
a newer build migrating its store never meets an older one. Every `YAGURA_*` variable other than
`YAGURA_INSTANCE` is removed from the daemon's environment.

After `--warmup` unrecorded repetitions per build, it runs `--reps` repetitions per build in the
order A B, B A, A B, ... Every repetition:

1. Starts `yagura-daemon` with a playbook that auto-approves `open_window` and `close_window`, and
   waits until it answers `ping`.
2. Opens one probe window (`hex_echo`: raw mode, echoes its input as hex) and attaches a view to
   it, then opens `--windows - 1` flood windows (`hex_echo --flood`), each writing `--flood-mib`
   MiB of 80-byte text lines and then a marker. Every window has its own attached view (120 x 30)
   that applies each frame and acknowledges it at once, as the GUI's frame loop does.
3. While the floods run, types a unique token into the probe every `--sample-gap` seconds and
   times it until its echo is on the probe's screen.
4. After the floods, waits `--settle`, samples idle CPU for `--idle-seconds`, then takes
   `--idle-samples` more latency samples.
5. Stops the daemon with `yagura-daemon --shutdown` for that instance. Only if it does not exit
   within 30 s is the PID this script started killed. No other process is touched.

The pseudo-child is a test fixture, not a vendor CLI, so the load does not depend on any CLI.

### Metrics

| Metric | Meaning |
|---|---|
| `throughput_mib_s` | Flood bytes / (last flood marker on screen - first flood window opened) |
| `latency_loaded_ms` | Median input-to-screen time of the probe while the floods run |
| `latency_idle_ms` | The same after the floods, with every window open and idle |
| `cpu_flood_s` | CPU seconds (user + kernel) of the process set over the flood |
| `cpu_idle_pct` | CPU of the process set while idle, in percent of one core |
| `mem_base_mib` | Working set of the process set before any window opens |
| `mem_loaded_mib` | Working set of the process set with every window open after the flood |
| `mem_per_window_mib` | (`mem_loaded_mib` - `mem_base_mib`) / `--windows` |
| `mem_private_loaded_mib` | Private bytes of the process set with every window open after the flood. Report only: not judged |
| `mem_private_per_window_mib` | (private bytes loaded - private bytes before any window) / `--windows`. Report only: not judged |

The working-set metrics count a page of a shared DLL once per process, so they read larger than
the memory the product adds. The two private metrics are reported beside them and take no part
in `WORSE` / `ok` or in the exit code of `compare`.

Each repetition's `detail` keeps the raw latency samples, the private bytes and a per-image
breakdown that also covers processes outside the set (`OpenConsole.exe`, the fixtures).

### The process set

CPU and memory are summed over the processes whose image name is in `--procs` (default
`yagura-daemon.exe,yagura-gui.exe,yagura-holder.exe`) **and** whose executable lives in the build's
staged directory. The directory filter keeps any other yagura (a resident daemon, an installed
build) out of the numbers. A name that is not running adds nothing, so one default set compares a
build without a holder against one with it. No GUI is started; the script's own client stands in
for the views.

## 3. Usage

```bash
python scripts/bench/bench.py run \
    --bin-dir D:/yagura-build/bench/bin-45c94f2 --label "main 45c94f2" \
    --bin-dir D:/yagura-build/target-t1051/release --label "holder" \
    --out D:/yagura-build/bench/holder.json
python scripts/bench/bench.py compare D:/yagura-build/bench/holder.json
```

One run of two builds with the defaults takes about 14 minutes. `compare RESULT` judges the second
build against the first (`--base` / `--new` pick builds by label) and exits 1 when any metric is
worse, 0 otherwise. `compare BASE NEW` with two separate result files only prints the medians and
exits 3: it does not judge (see §4). Daemon output goes to `<out stem>-work/`.

## 4. Repetitions and the judgement

Everything below was measured on one machine (Windows 11, 24 logical CPUs) with the release build
of `45c94f2`, 8 windows and 8 MiB per flood window.

### Why the two builds run interleaved in one run

Three separate runs of the same build gave throughput medians of 5.29, 5.37 and 5.83 MiB/s and
`cpu_flood_s` medians of 141.5, 141.4 and 129.6 s, a drift of about 10% between sessions, larger
than the spread inside any one run (robust CV 3-7%). Judged against each other, one same-build
pair came out "worse" on throughput and CPU. The cause of the drift was not identified. Measuring
both builds in alternating order inside one run and judging the per-repetition differences cancels
it; `compare` therefore does not judge two separate results.

### Why the binaries are staged

In two paired runs of identical binaries, the copy run from the cargo target directory used more
CPU than a plain copy in 16 of 20 pairs (+3.1% and +1.7% by median), whichever instance index it
had. Two sibling copies showed no such bias (signs mixed, -1.5%). The cause was not identified.
`run` therefore copies every build into sibling directories of its work directory. With staging,
the same target-versus-copy pair measured +1.5% with mixed signs.

### Why a warm-up repetition

The first daemon start from a freshly copied executable measured a base working set of 24.4 MiB,
later starts 11.7 MiB. `--warmup 1` discards one repetition per build.

### The judgement

For each metric and each repetition i, `d_i` is how much worse the new build was than the base in
the same pair (sign chosen so positive means worse). The new build is **worse** when

```
median(d) > max(floor, z * 1.2533 * 1.4826 * MAD(d) / sqrt(n))
```

1.4826 x MAD estimates the standard deviation of `d`, and 1.2533 x sigma / sqrt(n) is the standard
error of a median, so the second term is z standard errors (`--z`, default 3). The relative
standard deviation of `d` measured in the two staged / sibling A/A runs:

| Metric | sigma of d (A/A, % of median) |
|---|---|
| `throughput_mib_s` | 2.6-2.8 |
| `latency_loaded_ms` | 2.5-3.4 |
| `latency_idle_ms` | 24-45 |
| `cpu_flood_s` | 1.7-3.3 |
| `mem_base_mib` | 0.1-0.3 |
| `mem_loaded_mib` | 1.5-2.3 |
| `mem_per_window_mib` | 2.0-3.2 |

**R = 10.** With z = 3 the noise term is 1.19 sigma at R = 10. Together with the floors below,
the tolerance is about 2-5% of the median for throughput (3.1-3.3%), loaded latency (4-4.5%), CPU
(3.1-3.9%), loaded memory (1.8-2.7%) and memory per window (2.4-3.8%). R = 20 would shrink only the
noise term (by 1/sqrt(2)); the floors, which already set the tolerance for throughput, CPU and
loaded latency, would not move, at twice the time (28 minutes per comparison). `latency_idle_ms`
is too noisy at any practical R to detect less than about 30%; it is reported for gross
regressions only.

**Floors** (`METRICS` in `bench.py`) are, per metric, twice the largest absolute A/A median
difference seen, rounded up, and never below the instrument's resolution:

| Metric | Largest A/A median difference | Floor |
|---|---|---|
| `throughput_mib_s` | 0.09 MiB/s | 0.2 MiB/s |
| `latency_loaded_ms` | 0.009 ms | 0.02 ms |
| `latency_idle_ms` | 0.109 ms | 0.25 ms |
| `cpu_flood_s` | 1.94 s (staged / sibling runs; 3.96 s unstaged, see above) | 4.0 s |
| `cpu_idle_pct` | 0 (resolution 0.104%, see §5) | 0.25% |
| `mem_base_mib` | 0.020 MiB | 0.05 MiB |
| `mem_loaded_mib` | 0.24 MiB | 0.5 MiB (the noise term, about 1 MiB, dominates) |
| `mem_per_window_mib` | 0.029 MiB | 0.06 MiB |

All four paired A/A runs (two unstaged, two staged / sibling) are judged not worse, in both
directions. The closest was the unstaged target-versus-copy run on `cpu_flood_s`, at 0.94 of its
tolerance; the staged ones stayed at or below 0.48 on every metric. The floors were calibrated on
those same runs, so for the metrics where the floor sets the tolerance this is not independent
evidence. **A WORSE whose `worse_by` is below 1.5 x the tolerance is to be measured once more and
both results reported.**

## 5. What it does not measure

- **macOS and Linux.** `run` is Windows only.
- **The GUI.** Frames are applied by this script's client, which acknowledges faster than a
  WebView applies them. The daemon builds frames as fast as they are acknowledged, so its
  frame-building load here is an upper bound of the GUI's.
- **Idle CPU below 0.1% of a core.** `GetProcessTimes` advances in scheduler ticks (15.6 ms here),
  so over 15 s of idle it resolves 0.104% of a core; every A/A median read 0.
- **Vendor CLI output.** The flood is plain text lines; TUI redraws, colour, and terminal queries
  are not part of the load.
- **The cause of the drift between sessions and of the target-directory bias** (§4). Both are
  measured and worked around, not explained.
- **Detection of a real regression.** No build with a known regression was measured; the
  detectable change in §4 is derived from the A/A spread, not demonstrated.
