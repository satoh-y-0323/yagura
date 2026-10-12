# flake-harness

A single-file harness (`flake_harness.py`) that repeats one measurement arm, classifies every
iteration as **pass / hang / fail / build-failure**, captures thread and process state *before*
killing a hung target, and keeps the machine clean between iterations.

It exists to measure intermittent hangs and failures of the yagura Rust test suite under CPU
pressure. It is a developer tool: CI never calls it, and it never modifies repository sources.

## 1. Platform and prerequisites

- **Windows only.** Process snapshots, the process tree and the thread state come from PowerShell
  (`Get-CimInstance Win32_Process`, `Get-Process | Threads`). On any other OS the harness prints a
  message on stderr and exits with code 1 without doing anything.
- `python` on `PATH` (developed and measured with 3.11.9). **Standard library only** - nothing to
  install, no `psutil`.
- PowerShell 5.1 reachable as `powershell`. The harness only ever runs
  `powershell -NoProfile -Command <script>`; it never writes or executes a `.ps1`, so the default
  `Restricted` execution policy is fine.
- `cargo` on `PATH` for the cargo-backed axes.
- No administrator rights are required. `taskkill` is not used (see §8).

**Self diagnosis (fail-loud).** Every `run` starts by taking one process snapshot and one thread
table of the harness process itself. If either fails, the run stops **before any measurement**
with a non-zero exit code and `stage: self-check`. The same check is available on its own:

```bash
python scripts/flake-harness/flake_harness.py selfcheck
```

**Preconditions for a meaningful measurement**

- No resident `yagura-daemon` / `yagura-gui`: every process whose executable lives under the
  repository `target/` (or under a `--extra-target-dir`) belongs to the cleanup population and
  **will be terminated** between iterations.
- No other `cargo` invocation against the same target directory at the same time.
- Run the harness from the repository root (any cwd works, but the iteration command is executed
  with the repository root as its working directory).

**A process unrelated to the harness is never terminated, even if it appears while an iteration is
running.** Cleanup only ever touches (A) pids observed as the iteration's own descendant tree
(periodic sampling) and (B) executables under `target/`; anything else that survives an iteration
is reported in `hygiene.log` and the summary, never killed. See §8.

## 2. Subcommands

| Subcommand | Purpose |
|---|---|
| `run` | Run one measurement arm (`--iterations N`) and write logs, captures and the summary. |
| `sweep` | Standalone cleanup: terminate the leftover population and report what is left. |
| `selfcheck` | Run the PowerShell self diagnosis only. |

`python scripts/flake-harness/flake_harness.py <subcommand> --help` prints the flags.

## 3. The three measurement axes

Any combination of the three axes may be requested; the axes are independent flags.

**(A) target resolution - `--target`**

| Value | Meaning | Required flags |
|---|---|---|
| `cargo` | Resolve the test executable once with `cargo test -p <crate> --test <file> --no-run --message-format=json`, then run that executable every iteration. With `--form full` there is no resolution step: `cargo` itself is the iteration command. | `--crate`, `--test-file` (not with `--form full`) |
| `exe` | Use an already built executable; **no cargo step at all**. `resolve.log` is not created, and the summary shows the executable that was used. | `--exe` |
| `command` | Any executable with a free argument list (synthetic commands). | `--exe`, `--arg` (repeatable) |

**(B) execution form - `--form`**

| Value | Meaning |
|---|---|
| `single` | One test: the harness appends `<test name> --exact --test-threads 1`. With `--target command` no arguments are appended - the `--arg` list is passed verbatim and the iteration is one process. |
| `binary` | The whole test binary at libtest's default parallelism (no filter). |
| `full` | `cargo` is the iteration command. Default arguments: `test --workspace --no-fail-fast`; replace them with `--cargo-arg` (repeatable). Requires `--target cargo`. |

**(C) CPU saturation - `--saturate`**

Present = on. The harness starts one busy-loop process per logical core (`--saturate-workers N`
overrides the count) before the first iteration and stops them after the last one; the final
cleanup record lists each worker pid and its state.

**Valid combinations** - every (A)x(B)x(C) pair is accepted except: `--form full` needs
`--target cargo`; `--target command` needs `--form single` and takes no `--test-name`;
`--form single` needs `--test-name` for `--target cargo|exe`; `--form binary|full` takes no
`--test-name`. Invalid combinations exit with code 2 and an explanation, before any measurement.

## 4. Calling it (verbatim examples)

All examples are meant to be pasted into **Git Bash** from the repository root.

```bash
# (1) cargo resolution x single test x saturation on - the "one flaky test" arm
python scripts/flake-harness/flake_harness.py run --run-id measure-status-token --target cargo --form single --saturate --crate yagura-daemon --test-file status_token --test-name window_verify_status_token_accepts_the_configured_override --iterations 60

# (2) cargo resolution x whole binary at default parallelism x saturation on
python scripts/flake-harness/flake_harness.py run --run-id confirm-status-token --target cargo --form binary --saturate --crate yagura-daemon --test-file status_token --iterations 40

# (3) direct executable x single test x saturation on - the control arm (no cargo step)
python scripts/flake-harness/flake_harness.py run --run-id control-prefix --target exe --form single --saturate --exe 'C:/Users/you/scratch/prefix-target/debug/deps/status_token-1a2b3c4d5e6f7890.exe' --test-name window_verify_status_token_accepts_the_configured_override --iterations 60 --extra-target-dir 'C:/Users/you/scratch/prefix-target'

# (4) arbitrary executable and argument list x single x saturation off - synthetic commands
python scripts/flake-harness/flake_harness.py run --run-id demo-fail --target command --form single --exe python --arg=-c --arg 'print("synthetic fail"); raise SystemExit(1)' --iterations 2

# (5) full run (cargo test --workspace --no-fail-fast) x saturation off, 10 times
python scripts/flake-harness/flake_harness.py run --run-id full-run-10 --target cargo --form full --iterations 10

# (6) full run form with replaced cargo arguments (reduced scope)
python scripts/flake-harness/flake_harness.py run --run-id full-run-cli-args --target cargo --form full --cargo-arg test --cargo-arg=-p --cargo-arg yagura-daemon --cargo-arg=--test --cargo-arg cli_args --iterations 1

# (7) standalone cleanup and self diagnosis
python scripts/flake-harness/flake_harness.py sweep
python scripts/flake-harness/flake_harness.py selfcheck
```

### Passing arguments from Git Bash

- **Any value that starts with `-` must use the `--flag=value` form**, otherwise `argparse` reads it
  as another option: `--arg=-c`, `--cargo-arg=-p`, `--cargo-arg=--test`.
- **Never pass a token that starts with `/`.** MSYS rewrites it into a Windows path before Python
  sees it. The harness itself never needs such a token.
- Windows paths: prefer forward slashes inside single quotes (`'C:/Users/you/...'`). A backslash
  path also works if single quoted (`'C:\Users\you\...'`), but `\U` etc. inside double quotes is a
  trap.
- Values containing spaces, semicolons or quotes go inside single quotes, as in example (4).
  A synthetic command that itself needs single quotes can build them with `chr(39)` so that the
  whole argument stays free of single quotes.
- `--run-id` is optional (default `run-<timestamp>`) but **give it explicitly when you start the
  harness in the background**: the output directory is then known before the process starts.
  Must match `[A-Za-z0-9._-]{1,64}`, be neither `.` nor `..`, and not be a Windows reserved device
  name (`CON` / `NUL` / `COM1` etc.) — it becomes a single directory name component under the log
  base, never a path.

### Long runs: start once in the background and poll

One harness process runs all iterations sequentially - never start several harness processes at
the same time. For long arms (a 10x full run, a 60x measurement) start a single background process
and poll its progress file:

```bash
# start (single process, all 60 iterations)
python scripts/flake-harness/flake_harness.py run --run-id measure-status-token --target cargo --form single --saturate --crate yagura-daemon --test-file status_token --test-name window_verify_status_token_accepts_the_configured_override --iterations 60 > /c/Users/you/AppData/Local/Temp/measure.out 2>&1

# poll: last progress line, and completion
tail -2 target/flake-logs/measure-status-token/progress.jsonl
ls target/flake-logs/measure-status-token/summary.txt
```

`summary.txt` is written exactly once, at the very end (also on a fail-loud stop), so its existence
is the completion signal. The last line of `progress.jsonl` is `"event": "run-end"` for a completed
run and `"event": "run-stopped"` for a stop.

## 5. Output files

Everything of one run lands in `<log base>/<run id>/`. The log base defaults to
`target/flake-logs` under the repository root (`/target/` is git-ignored); `--log-base` overrides
it. All paths printed in the summary are absolute. A run id must be fresh: if the directory already
exists and is not empty the harness refuses to start (exit code 1, nothing is measured, no summary
is written).

| File | Content |
|---|---|
| `summary.txt` | Final report: axes, iteration command, **completed iterations**, the four classification counts with the iteration numbers of each class, per-iteration table, capture file list, libtest aggregation, the three extraction items, hygiene/cleanup counters, residual processes and the **unattributed new survivor processes** (reported, never terminated). Also printed on stdout. |
| `progress.jsonl` | One JSON object per event, appended as it happens: `run-start`, `self-check`, `resolve-start`, `target-resolved`, `saturation-start`, `iteration-start`, `iteration-end`, `hygiene`, `budget-stop`, `stop`, `saturation-stop`, `run-end` / `run-stopped`. `iteration-start` carries the sampling interval in effect; `iteration-end` carries the classification, return code, duration, log path, capture path, libtest counters, the **unattributed survivor pids of that iteration** (`unattributed_survivor_pids`) and `completed_iterations`. |
| `iter-<NNN>.log` | **One log per iteration**, with the target's stdout and stderr merged into the same file at the OS level, so cargo's `Running` lines and libtest's result / 60-second warning lines keep their relative order. |
| `capture-iter-<NNN>.txt` | Written **only for hang iterations, before anything is killed**: the process tree (pid / ppid / name / executable path / command line / creation time) of the target and all its descendants, followed by the thread table (`Id ThreadState WaitReason TotalProcessorTime`) of every live process in that tree. One file per iteration; the summary lists them with absolute paths. |
| `resolve.log` | Merged output of the `cargo ... --no-run --message-format=json` resolution step (only for `--target cargo` with `--form single|binary`). |
| `hygiene.log` | One line per pre-iteration cleanup decision (which process was terminated, why it was in the population, whether it died) plus one line per **unattributed new survivor** found at the end of an iteration (`unattributed new survivor (not terminated) ...`) - those are reported only, never touched. |
| `cleanup-final.txt` | Shutdown record: every saturation worker with its final state, the cleanup population at shutdown, and `residual processes: N` with the details of anything that survived. |
| `leftovers.json` | Pids (with creation time) observed as the iteration root or one of its descendants (population (A)) that the harness still considered its own at the end; `sweep --from-run <dir>` can read it back. |
| `selfcheck.txt` | Result of the start-up diagnosis and the protected pid list. |
| `sweep-<stamp>.txt` | Record of a standalone `sweep` (default location: the log base). |

## 6. Classification and stop rules

Per iteration, in this order:

1. **hang** - the iteration deadline was reached. The harness first writes `capture-iter-<NNN>.txt`
   (tree + threads), then terminates the target and its descendants, then continues with the next
   iteration. **Hangs never end the run early**; only the time budget does.
2. **build-failure** - only possible when the iteration command *is* cargo (`--form full`): exit
   code non-zero, no `test result:` line anywhere in the log, and at least one line starting with
   `error:` or `error[Ennnn]:`. Iterations that run an already built test binary can never be
   classified this way.
3. **pass** - exit code 0.
4. **fail** - any other non-zero exit code.

**Stop (fail-loud, not a classification)** - the run ends with exit code 1 and `summary.txt`
contains a `STOPPED (fail-loud)` block with the stage, the message and the number of completed
iterations (`0` when no iteration could be started):

| Stage | Cause |
|---|---|
| `self-check` | The PowerShell diagnosis failed before any measurement. |
| `resolve` | `cargo ... --no-run` failed (build or link error, unknown target): see `resolve.log`. |
| `snapshot` | A process snapshot failed or returned invalid data. |
| `hygiene` | A process in the cleanup population could not be terminated. |
| `kill` | A hung target survived termination. |
| `cleanup` | Something survived the final cleanup (also listed in `cleanup-final.txt`). |

**Exit codes**: `0` the run finished (whatever the classifications were), `1` fail-loud stop,
`2` invalid arguments.

**Time budget** - `--budget-seconds` (default 14400 = 4 h) is checked *between* iterations. When it
is exceeded the run stops iterating, the summary notes the budget stop, and the exit code stays 0.
A single iteration can therefore overshoot the budget by at most one iteration deadline.

## 7. libtest aggregation and extraction

Every iteration log is parsed by pairing each `Running <artifact>` / `Doc-tests <name>` line with
the next `test result: ...` line. The summary reports `suites / passed / failed / ignored` per
iteration and as a total, plus three separate extraction items with per-name counts:

- **FAILED test names** - lines `test <name> ... FAILED`.
- **over-60s warned without result line** - names from `test <name> has been running for over 60
  seconds` that never got an `ok` / `FAILED` / `ignored` line (i.e. tests that were still stuck).
- **Running without test result line** - suites whose `Running` line has no matching `test result:`
  line (a binary that died or hung before printing its result).

For a direct test binary (`--target exe|cargo` with `--form single|binary`) there is no `Running`
line; such a suite is reported as `(direct)`.

## 8. Cleanup

**Which processes are cleaned.** Termination is restricted to the union of two sets - nothing
else is ever touched, however long it survived an iteration:

- **(A) observed descendants.** While an iteration runs, the harness takes a full process-table
  snapshot every `--sample-interval-seconds` (default 3 s, also right after the target is spawned
  and once more right when it finishes) and records the iteration's target pid together with every
  pid reachable from it through recorded parent links **at that moment**. A pid stays attributed
  even if its immediate parent exits before the next sample - once a chain was intact for a single
  snapshot the whole branch below it is remembered, so a grandchild whose parent exits almost
  immediately is still caught as long as one sample lands while the parent was alive. This
  accumulated set (persisted in `leftovers.json`) is the population's first half; it is name-blind,
  it never looks at process names.
- **(B) target-dir executables.** Every process whose executable path resolves under the
  repository `target/` (or a `--extra-target-dir`), checked directly against a live snapshot -
  independent of any observation history.

A process that survives an iteration but belongs to **neither** set - because it was launched by
something else entirely (a user's terminal, an unrelated background job) whose own ancestor chain
never ran through the iteration's target - is **never terminated**. It is logged in `hygiene.log`
as `unattributed new survivor (not terminated) ...` and listed in `summary.txt` and in the
`iteration-end` event's `unattributed_survivor_pids`, so it can be investigated by hand.

**When it runs.** Before every iteration (including the very first, and including runs that use a
directly specified executable) population (A) union (B) - the accumulated observations of
previous iterations included - is checked, terminated and recorded. If something cannot be
terminated the run stops fail-loud. After the last iteration the saturation workers are stopped
and the same population is swept once more into `cleanup-final.txt`, which ends with
`residual processes: N`.

**Why not a before/after diff.** An earlier version identified the population from the process
table diff between the start and the end of an iteration (whatever appeared and was still alive).
That missed *who* a survivor belonged to: a process whose own launcher had already exited by the
time of the diff (an intermediate shell spawned by an unrelated tool, for example) looks exactly
like an orphan of the measured target, and got killed. Sampling **during** the iteration, not just
before and after it, is what lets the harness tell the two apart.

**Standalone cleanup.**

```bash
python scripts/flake-harness/flake_harness.py sweep
python scripts/flake-harness/flake_harness.py sweep --from-run target/flake-logs/measure-status-token --extra-target-dir 'C:/Users/you/scratch/prefix-target' --pid 12345
```

It prints the population, terminates it and ends with `residual processes: 0` (exit code 0) or a
`STILL ALIVE` list (exit code 1); the same text is written to `sweep-<stamp>.txt`.

**If termination is refused.** The harness uses PowerShell `Stop-Process -Force` and never calls
`taskkill` (it can be denied inside an agent sandbox). If a process is reported as `STILL ALIVE`,
finish it from your own terminal:

```
taskkill /F /T /PID <pid>
```

and re-run `sweep` to confirm `residual processes: 0` before starting the next measurement.

## 9. Defaults

| Setting | Default | Flag |
|---|---|---|
| Iteration deadline | 120 s (`--form single` / `binary`), 2400 s = 40 min (`--form full`) | `--timeout-seconds` |
| Total time budget | 14400 s = 4 h, checked between iterations | `--budget-seconds` |
| Iterations | 1 | `--iterations` |
| Log base | `<repo>/target/flake-logs` | `--log-base` |
| Run id | `run-<YYYYmmdd-HHMMSS>`, or `[A-Za-z0-9._-]{1,64}` (not `.`/`..`, not a Windows reserved device name such as `CON` / `NUL` / `COM1`) if given | `--run-id` |
| Saturation workers | number of logical cores | `--saturate-workers` |
| Cargo arguments (`--form full`) | `test --workspace --no-fail-fast` | `--cargo-arg` |
| Sample interval (population (A)) | 3 s | `--sample-interval-seconds`, env `FLAKE_HARNESS_SAMPLE_INTERVAL_SECONDS` |

The deadlines are cut-off values, not expectations: 120 s is chosen because the known hang symptom
blocks for more than 60 s, and 40 min is twice the slowest healthy full run measured on the
development machine.
