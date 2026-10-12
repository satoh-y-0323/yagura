# stash-rebuild

A single-file developer command (`stash_rebuild.py`) that renames the executables a running
process holds open out of the way, runs `cargo build`, and cleans the renamed leftovers up on a
later run.

It exists because on Windows a file that a process has open as its running image cannot be
deleted: while the parent session's MCP bridge (`yagura-mcp.exe`) or a resident
`yagura-daemon.exe` is running, `cargo build` fails with `failed to remove file ...
yagura-mcp.exe` / `os error 5`. Renaming that same file *is* allowed - the holding process keeps
running from the renamed file - so the build can link a fresh binary into the now free name.

This file is the single source for how to call the command; `docs/building.md` only points
here.

## 1. Platform and prerequisites

- **Windows.** On any other OS the command builds without stashing or sweeping anything: there,
  a running executable can simply be replaced.
- `python` on `PATH` (developed with 3.11.9). **Standard library only** - nothing to install.
- `cargo` on `PATH`.
- No administrator rights. No process is ever terminated - the point of the command is that the
  holding process keeps running.

**The new binary is not picked up by the process that holds the old one.** That process keeps
executing the stashed file until it is restarted; the rebuilt binary takes effect at its **next
start**.

## 2. Calling it (verbatim examples)

All examples are run from anywhere - the workspace is derived from the script's own location, not
from the current directory.

```bash
# (1) rebuild the MCP bridge while the parent session holds it open
python scripts/stash-rebuild/stash_rebuild.py --bin yagura-mcp -- -p yagura-mcp

# (2) rebuild the daemon while the GUI keeps it running
python scripts/stash-rebuild/stash_rebuild.py --bin yagura-daemon -- -p yagura-daemon --bins

# (3) several targets at once, release profile
python scripts/stash-rebuild/stash_rebuild.py --bin yagura-mcp --bin yagura-daemon --release -- --workspace
```

| Argument | Meaning |
|---|---|
| `--bin NAME` | Executable to stash before building, **without extension**. Repeatable, required at least once. Must be a single path component made of letters, digits, `_`, `.` and `-` only; `""`, `.` and `..` are refused as well. Anything else is rejected before a file is touched. |
| `--release` | Use the `release` profile directory instead of `debug`. Also passed to `cargo build`. |
| `-- <args>` | Everything after `--` is passed on to `cargo build` verbatim (this is where `-p` goes). |

Exit codes: `0` the build succeeded and every stashed target is back under its own name, the
build's own exit code when the build failed, `1` a stash failure, a `cargo metadata` failure, or a
stashed target that the build did not put back under its own name (§7), `2` invalid arguments.

**`--bin` and the cargo arguments are not cross-checked before the build.** What to stash and what
to build are two different axes: one build can produce several binaries, and a crate name need not
match a binary name, so a check on the argument list alone would have to reproduce cargo's own
argument handling to tell a legitimate combination from a mistaken one. **The mismatch is caught
from the result instead**: a target the build did not produce is still absent from its own name
when the build ends, so its stash is moved back and the command reports it and exits non-zero (§3
step 6, §7).

**Every target named with `--bin` must therefore be produced by the build.** `--bin yagura-mcp
--bin yagura-daemon -- -p yagura-mcp` is *not* a supported combination: `yagura-daemon` is stashed
but never rebuilt, so it is restored to the state it had before the run and reported as an error.
Name only the targets that this build produces, or widen the build (`--workspace`, a second `-p`)
to cover them.

The target directory comes from `cargo metadata` (`target_directory`), so `CARGO_TARGET_DIR` and
`[build] target-dir` in `.cargo/config.toml` are honoured. If `cargo metadata` fails, the command
stops instead of guessing a path.

## 3. What it does, in order

1. Resolve the workspace manifest from the script's own location and read `target_directory` from
   `cargo metadata`. Both `cargo metadata` and `cargo build` run with the repository root as their
   working directory, so a `.cargo/config.toml` next to the caller cannot leak into the build.
2. **Sweep**: delete the stash artifacts of the named targets in `<target_directory>/<profile>/`
   that are left over from earlier runs (§4).
3. **Stash**: rename `<target_directory>/<profile>/<bin>.exe` to
   `<bin>.exe.locked-<YYYYMMDD-HHMMSS>` (local time; a `-2`, `-3`, ... suffix is added if that name
   is taken). A target that does not exist yet is skipped - a first build needs no stashing.
   **If a rename fails, `cargo build` is not started, the targets stashed so far are rolled back
   (§5) and the command exits non-zero.**
4. Run `cargo build [--release] <extra args>`.
5. If the build failed, roll back (§5).
6. **If the build succeeded, check that every target stashed in step 3 exists again under its
   original name.** A target that does not - because this build never produced it, or because its
   output went to another path - is rolled back (§5) so that its original name is not left empty,
   then named on stderr, and the command exits non-zero (§7).
7. Print a summary: what was stashed, how many stash artifacts were recovered, how many are still
   in use (with the oldest one's date), and whether a rollback happened.

## 4. Stash artifacts and how they are cleaned up

**Leftovers are normal, not a failure.** The process that holds the old executable keeps holding
the stashed file, so it cannot be deleted while that process lives. It is deleted by the **next**
run of the command after that process has been restarted. There is no upper limit on how many
leftovers are tolerated: stopping on a threshold would leave no way out short of killing the
process.

The sweep only touches files that belong to the targets named with `--bin`. Three forms are
recognised, each optionally with a `-<n>` collision suffix:

| Form | Origin |
|---|---|
| `<bin>.exe.locked-<YYYYMMDD-HHMMSS>` | this command |
| `<bin>.locked-<pid>.exe` | earlier manual stashes done by hand |
| `<bin>.exe.old-<YYYYMMDD-HHMMSS>` | the form used while measuring the problem |

Normal build products (`<bin>.exe`, `<bin>.pdb`, `<bin>.d`) and the stash artifacts of *other*
binaries are never touched.

**Deletion uses Python's `os.remove` only, never an external `rm`.** The MSYS2 `rm` cannot POSIX
delete an in-use executable and falls back to moving it into `C:\$Recycle.Bin`, where a 6 MB
binary then sits with its original size and date. Sweeping with `rm` would fill the user's recycle
bin instead of cleaning up.

## 5. Rollback

A rollback moves targets stashed **by the current run** back to their original names - **but only
where that name is free**. Cargo may have already uplifted a fresh binary into place; restoring the
stash there would replace the new binary with the old one, so those stashes are kept instead and
the summary says so. Stash artifacts left over from *earlier* runs are never rolled back.

It fires in three situations, all under that same condition:

| Trigger | Effect |
|---|---|
| A rename in step 3 fails partway through | The targets already stashed in this run are moved back, `cargo build` is never started, exit code 1. The target whose rename failed was never stashed, so it is untouched. |
| `cargo build` exits non-zero | The targets stashed in this run are moved back, and the build's exit code is passed on. |
| The build succeeded but a stashed target is missing from its own name (§3 step 6) | **Only the missing targets** are moved back - a target the build did rebuild keeps its new binary - and exit code 1. |

The summary names the targets that were moved back (`rollback: restored ...`) separately from
those that could not be (`rollback: NOT restored ...`, when the original name is occupied or the
move itself failed), so the reader can tell which targets are back in place.

Leaving a target stashed in any of these cases would take it out of the name that other tools
resolve - the MCP registration points at `target/<profile>/<bin>.exe` - so no failure path ends
with a target stashed while its own name is free.

## 6. Tests

```bash
python -m pytest scripts/stash-rebuild/ -v
```

`test_stash_rebuild.py` is the executable contract for the module's public API: stash name
generation, the recovery-target decision, path resolution, the stash / sweep / rollback file
operations (all on `tmp_path`, never on the repository's `target/`) and `main()` end to end with
`cargo metadata` and `cargo build` injected. Run it by hand after changing the script; it is **not**
part of CI.

## 7. Out of scope

- **`--target`, `--target-dir` and `--profile` in the cargo arguments are rejected** (exit code 2,
  nothing is stashed). They move the build output to a path other than the one that was stashed,
  which would silently leave the two out of sync. Use `--release` for the release profile; for a
  cross-compiled `--target` or a redirected `--target-dir` build, stop the holding process instead.

  **Rejecting those names is early detection, not the guarantee.** `cargo build --config
  build.target-dir="..."` moves the output exactly the same way without naming any of them, and
  `--config` can override any cargo setting, so no list of argument names can ever be complete.
  The guarantee comes from the result instead: after a successful build the command checks that
  each stashed target is back under its original name (§3 step 6). A target that did not come back
  is rolled back to the state it had before the run (§5), named on stderr, and the command exits
  non-zero, rather than reporting a success that left the target missing from the name other tools
  resolve. The check cannot tell a redirected output path from a target this build never produced,
  so both end the same way; restoring first keeps the harmless case (a `--bin` the build does not
  cover) from vacating a name that was occupied before the run.
- **The Tauri sidecar copy is not covered.** `gui/scripts/prepare-sidecar.mjs` stages
  `yagura-daemon.exe` into `gui/src-tauri/binaries/`, outside `cargo metadata`'s
  `target_directory`, and runs its own `cargo build --release -p yagura-daemon`, so it never goes
  through this command. The daemon **inside** the target directory is covered by
  `--bin yagura-daemon`.
- **Nothing is ever stopped or killed.** If a rebuild must actually take effect immediately, the
  holding process has to be restarted by hand.
- **Running a second, named instance (see `docs/instances.md`) does not make this command
  unnecessary.** `--bin yagura-mcp` stays needed regardless of instances: `.mcp.json` registers
  `./target/debug/yagura-mcp.exe`, and the process that holds that file open is the Claude Code
  session developing this repository itself -- not a yagura instance -- so separating instances
  changes nothing about it. `--bin yagura-daemon` becomes unnecessary only while your work instance
  runs an **installed** build; it stays needed whenever a development instance (`YAGURA_INSTANCE`
  set, running `target/debug/yagura-daemon.exe`) is itself the executable being rebuilt, since that
  instance then holds its own file open the same way the single daemon does today.
