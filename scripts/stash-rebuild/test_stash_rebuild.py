"""Contract tests for scripts/stash-rebuild/stash_rebuild.py.

Run with: python -m pytest scripts/stash-rebuild/ -v

This file is also the executable contract for stash_rebuild.py's public API.
The public names used below are what stash_rebuild.py must provide:

  generate_stash_name(bin_name, ext, timestamp, existing_names) -> str
  is_stash_artifact(filename, bin_name, ext) -> bool
  resolve_target_path(target_directory, profile, bin_name, ext) -> Path
  stash_target(target_path, stash_path) -> bool          # False = no-op
  sweep(directory, bin_name, ext) -> SweepResult          # .removed / .remaining (List[Path])
  rollback(stashed, originals) -> RollbackOutcome         # .restored / .left_in_place (List[str])
  resolve_workspace_manifest() -> Path                    # repo root Cargo.toml, cwd-independent
  main(argv, *, metadata_fn=None, build_fn=None) -> int
    metadata_fn(manifest_path: Path) -> dict (with "target_directory" key), raises on failure
    build_fn(cargo_args: Sequence[str], cwd: Path) -> int (cargo's exit code)
"""

from __future__ import annotations

import os
import subprocess
from datetime import datetime
from pathlib import Path

import pytest

import stash_rebuild

STASH_TIMESTAMP = "20260911-000000"


# ---------------------------------------------------------------------------
# 1. Stash name generation
# ---------------------------------------------------------------------------


def test_generate_stash_name_uses_locked_prefix_and_local_timestamp():
    ts = datetime(2026, 9, 11, 12, 34, 56)
    name = stash_rebuild.generate_stash_name("yagura-mcp", ".exe", ts, existing_names=set())
    assert name == "yagura-mcp.exe.locked-20260911-123456"


def test_generate_stash_name_empty_extension_format():
    ts = datetime(2026, 9, 11, 12, 34, 56)
    name = stash_rebuild.generate_stash_name("yagura-mcp", "", ts, existing_names=set())
    assert name == "yagura-mcp.locked-20260911-123456"


def test_generate_stash_name_avoids_collision_with_sequential_suffix():
    ts = datetime(2026, 9, 11, 12, 34, 56)
    base = "yagura-mcp.exe.locked-20260911-123456"
    name = stash_rebuild.generate_stash_name("yagura-mcp", ".exe", ts, existing_names={base})
    assert name != base
    assert name not in {base}
    assert name.startswith(base)


def test_generate_stash_name_collision_chain_skips_taken_suffixes():
    ts = datetime(2026, 9, 11, 12, 34, 56)
    base = "yagura-mcp.exe.locked-20260911-123456"
    existing = {base, f"{base}-2"}
    name = stash_rebuild.generate_stash_name("yagura-mcp", ".exe", ts, existing_names=existing)
    assert name not in existing


def test_generate_stash_name_never_equals_target_name_with_extension():
    ts = datetime(2026, 9, 11, 12, 34, 56)
    name = stash_rebuild.generate_stash_name("yagura-mcp", ".exe", ts, existing_names=set())
    assert name != "yagura-mcp.exe"


def test_generate_stash_name_never_equals_target_name_with_empty_extension():
    # Unix-style executables have no extension; the generated name must still
    # be distinguishable from the bare binary name.
    ts = datetime(2026, 9, 11, 12, 34, 56)
    name = stash_rebuild.generate_stash_name("yagura-mcp", "", ts, existing_names=set())
    assert name != "yagura-mcp"


# ---------------------------------------------------------------------------
# 2. Stash-artifact identification
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "filename",
    [
        f"yagura-mcp.exe.locked-{STASH_TIMESTAMP}",  # form 1: this tool's own stash name
        f"yagura-mcp.exe.locked-{STASH_TIMESTAMP}-2",  # form 1 with sequential suffix
        "yagura-mcp.locked-27616.exe",  # form 2: legacy manual stash (seen in target/debug)
        "yagura-mcp.locked-27616-2.exe",  # form 2 with sequential suffix
        f"yagura-mcp.exe.old-{STASH_TIMESTAMP}",  # form 3
        f"yagura-mcp.exe.old-{STASH_TIMESTAMP}-2",  # form 3 with sequential suffix
    ],
)
def test_is_stash_artifact_recognizes_all_three_forms_and_sequential_suffix(filename):
    assert stash_rebuild.is_stash_artifact(filename, "yagura-mcp", ".exe") is True


@pytest.mark.parametrize(
    "filename",
    [
        "yagura-mcp.exe",  # normal build artifact
        "yagura-mcp.pdb",  # normal build artifact
        "yagura-mcp.d",  # normal build artifact
        "yagura-mcp",  # extension-less normal build artifact
        f"yagura-daemon.exe.locked-{STASH_TIMESTAMP}",  # stash of a different target
        "yagura-daemon.locked-27616.exe",  # legacy stash of a different target
    ],
)
def test_is_stash_artifact_rejects_normal_artifacts_and_other_targets(filename):
    assert stash_rebuild.is_stash_artifact(filename, "yagura-mcp", ".exe") is False


def test_is_stash_artifact_handles_empty_extension():
    assert stash_rebuild.is_stash_artifact(f"yagura-mcp.locked-{STASH_TIMESTAMP}", "yagura-mcp", "") is True
    assert stash_rebuild.is_stash_artifact("yagura-mcp", "yagura-mcp", "") is False
    assert stash_rebuild.is_stash_artifact("yagura-mcp.d", "yagura-mcp", "") is False


# ---------------------------------------------------------------------------
# 3. Target path resolution
# ---------------------------------------------------------------------------


def test_resolve_target_path_joins_target_dir_profile_bin_ext(tmp_path):
    target_dir = tmp_path / "target"
    result = stash_rebuild.resolve_target_path(target_dir, "debug", "yagura-mcp", ".exe")
    assert result == target_dir / "debug" / "yagura-mcp.exe"


def test_resolve_target_path_empty_extension(tmp_path):
    target_dir = tmp_path / "target"
    result = stash_rebuild.resolve_target_path(target_dir, "release", "yagura-mcp", "")
    assert result == target_dir / "release" / "yagura-mcp"


def test_resolve_target_path_does_not_assume_target_directory_name(tmp_path):
    custom_dir = tmp_path / "my-custom-build-output"
    result = stash_rebuild.resolve_target_path(custom_dir, "debug", "yagura-mcp", ".exe")
    assert result == custom_dir / "debug" / "yagura-mcp.exe"


# ---------------------------------------------------------------------------
# 4. Performing the stash
# ---------------------------------------------------------------------------


def test_stash_target_moves_existing_file(tmp_path):
    target = tmp_path / "yagura-mcp.exe"
    target.write_bytes(b"binary")
    stash_path = tmp_path / "yagura-mcp.exe.locked-20260911-123456"
    moved = stash_rebuild.stash_target(target, stash_path)
    assert moved is True
    assert not target.exists()
    assert stash_path.read_bytes() == b"binary"


def test_stash_target_noop_when_target_missing(tmp_path):
    target = tmp_path / "yagura-mcp.exe"
    stash_path = tmp_path / "yagura-mcp.exe.locked-20260911-123456"
    moved = stash_rebuild.stash_target(target, stash_path)
    assert moved is False
    assert not stash_path.exists()


def test_stash_target_raises_when_destination_already_exists(tmp_path, monkeypatch):
    # os.rename() only refuses an occupied destination on Windows; POSIX
    # renames silently overwrite it. Fake a POSIX-style rename so this
    # contract is observed from stash_target's own pre-flight check rather
    # than from os.rename's platform-specific exception.
    def posix_style_rename(src: str, dst: str) -> None:
        Path(dst).write_bytes(Path(src).read_bytes())
        Path(src).unlink()

    monkeypatch.setattr(os, "rename", posix_style_rename)

    target = tmp_path / "yagura-mcp.exe"
    target.write_bytes(b"binary")
    stash_path = tmp_path / "yagura-mcp.exe.locked-20260911-123456"
    stash_path.write_bytes(b"already-there")
    with pytest.raises(OSError):
        stash_rebuild.stash_target(target, stash_path)
    # The faked rename must never have run: the pre-existing destination
    # content is untouched.
    assert stash_path.read_bytes() == b"already-there"


# ---------------------------------------------------------------------------
# 5. Performing the sweep
# ---------------------------------------------------------------------------


def test_sweep_removes_matching_artifacts_and_leaves_others(tmp_path):
    own = tmp_path / "yagura-mcp.exe.locked-20260911-000000"
    own.write_bytes(b"x")
    other_bin = tmp_path / "yagura-daemon.exe.locked-20260911-000000"
    other_bin.write_bytes(b"x")
    normal = tmp_path / "yagura-mcp.exe"
    normal.write_bytes(b"x")

    result = stash_rebuild.sweep(tmp_path, "yagura-mcp", ".exe")

    assert own in result.removed
    assert not own.exists()
    assert other_bin.exists()
    assert normal.exists()


def test_sweep_reports_undeletable_artifacts_without_raising(tmp_path):
    # A directory in place of a stash artifact cannot be removed with
    # os.remove (the sweep never falls back to shutil.rmtree/rm); this
    # simulates a "cannot delete" outcome without needing to hold an OS
    # file lock.
    blocked = tmp_path / "yagura-mcp.exe.locked-20260911-000000"
    blocked.mkdir()

    result = stash_rebuild.sweep(tmp_path, "yagura-mcp", ".exe")

    assert blocked in result.remaining
    assert blocked not in result.removed
    assert blocked.exists()


# ---------------------------------------------------------------------------
# 7. Rollback conditions (unit level)
# ---------------------------------------------------------------------------


def test_rollback_restores_when_original_missing(tmp_path):
    stash_path = tmp_path / "yagura-mcp.exe.locked-20260911-000000"
    stash_path.write_bytes(b"old-binary")
    original_path = tmp_path / "yagura-mcp.exe"

    outcome = stash_rebuild.rollback({"yagura-mcp": stash_path}, {"yagura-mcp": original_path})

    assert outcome.restored == ["yagura-mcp"]
    assert original_path.exists()
    assert original_path.read_bytes() == b"old-binary"
    assert not stash_path.exists()


def test_rollback_leaves_stash_when_original_already_exists(tmp_path):
    stash_path = tmp_path / "yagura-mcp.exe.locked-20260911-000000"
    stash_path.write_bytes(b"old-binary")
    original_path = tmp_path / "yagura-mcp.exe"
    original_path.write_bytes(b"new-binary-from-cargo-uplift")

    outcome = stash_rebuild.rollback({"yagura-mcp": stash_path}, {"yagura-mcp": original_path})

    assert outcome.left_in_place == ["yagura-mcp"]
    assert original_path.read_bytes() == b"new-binary-from-cargo-uplift"
    assert stash_path.exists()
    assert stash_path.read_bytes() == b"old-binary"


# ---------------------------------------------------------------------------
# 10. Working-directory independence
# ---------------------------------------------------------------------------


def test_resolve_workspace_manifest_points_to_repo_root_cargo_toml():
    manifest = stash_rebuild.resolve_workspace_manifest()
    assert manifest.name == "Cargo.toml"
    assert manifest.is_absolute()
    # Anchored to this script's own location, not to the interpreter's cwd.
    assert manifest.parent == Path(stash_rebuild.__file__).resolve().parent.parent.parent


def test_main_resolves_same_workspace_from_any_cwd(tmp_path, monkeypatch):
    target_dir = tmp_path / "target"
    (target_dir / "debug").mkdir(parents=True)
    seen_manifest_paths = []

    def fake_metadata(manifest_path):
        seen_manifest_paths.append(manifest_path)
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 0

    cwd_a = tmp_path / "a"
    cwd_b = tmp_path / "b" / "nested"
    cwd_a.mkdir()
    cwd_b.mkdir(parents=True)

    monkeypatch.chdir(cwd_a)
    stash_rebuild.main(["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build)
    monkeypatch.chdir(cwd_b)
    stash_rebuild.main(["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build)

    assert len(seen_manifest_paths) == 2
    assert seen_manifest_paths[0] == seen_manifest_paths[1]
    assert seen_manifest_paths[0] == stash_rebuild.resolve_workspace_manifest()


# ---------------------------------------------------------------------------
# main() end-to-end: performing the stash / performing the sweep / profile selection
# ---------------------------------------------------------------------------


def test_main_stashes_existing_target_before_build(tmp_path):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original = debug_dir / "yagura-mcp.exe"
    original.write_bytes(b"binary")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        # cargo has not uplifted a new binary yet; the original name must be
        # free for the linker to write to.
        assert not original.exists()
        # A real cargo build writes the fresh binary back under the original
        # name; simulate that so this stays a realistic successful run (the
        # post-build existence check in section 12 expects this).
        original.write_bytes(b"rebuilt-binary")
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code == 0
    stashed = list(debug_dir.glob("yagura-mcp.exe.locked-*"))
    assert len(stashed) == 1
    assert stashed[0].read_bytes() == b"binary"


def test_main_noop_stash_when_target_does_not_exist_yet(tmp_path):
    target_dir = tmp_path / "target"
    (target_dir / "debug").mkdir(parents=True)

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code == 0


def test_main_sweeps_recoverable_leftovers_from_previous_run(tmp_path):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    leftover = debug_dir / "yagura-mcp.exe.locked-20260101-000000"
    leftover.write_bytes(b"stale")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code == 0
    assert not leftover.exists()


def test_main_uses_release_profile_directory_when_release_flag_set(tmp_path):
    target_dir = tmp_path / "target"
    release_dir = target_dir / "release"
    release_dir.mkdir(parents=True)
    original = release_dir / "yagura-mcp.exe"
    original.write_bytes(b"binary")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        # Simulate cargo writing the rebuilt binary back to the original
        # name (see the post-build existence check in section 12).
        original.write_bytes(b"rebuilt-binary")
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--release"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code == 0
    stashed = list(release_dir.glob("yagura-mcp.exe.locked-*"))
    assert len(stashed) == 1


# ---------------------------------------------------------------------------
# 6. Stopping on failure
# ---------------------------------------------------------------------------


def test_main_stops_before_build_when_stash_fails(tmp_path, monkeypatch):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    (debug_dir / "yagura-mcp.exe").write_bytes(b"binary")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    build_calls = []

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    def failing_stash(*args, **kwargs):
        raise OSError("simulated stash failure")

    monkeypatch.setattr(stash_rebuild, "stash_target", failing_stash)

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    assert build_calls == []


# ---------------------------------------------------------------------------
# 6b. Rollback on a mid-way failure across multiple targets (coordinator's
# added contract, 2026-09-11)
#
# The rollback so far only covers a cargo build failure. If stashing several
# --bin targets fails partway through (a later target's stash fails), the
# targets stashed before it stay stashed and the build never starts. This is
# the same shape of problem rollback already protects against (a target's
# original name must not be left empty), so this requires restoring the
# targets already stashed in this run under the same condition (only when
# the original name is free) before exiting non-zero, even on a mid-way
# failure.
# ---------------------------------------------------------------------------


def test_main_rolls_back_earlier_stash_when_a_later_target_fails_to_stash(tmp_path, monkeypatch):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original_a = debug_dir / "yagura-mcp.exe"
    original_a.write_bytes(b"binary-a")
    original_b = debug_dir / "yagura-daemon.exe"
    original_b.write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    build_calls = []

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    real_stash_target = stash_rebuild.stash_target

    def flaky_stash_target(target_path, stash_path):
        # yagura-mcp (processed first) succeeds; yagura-daemon (processed
        # second) fails, simulating a failure partway through a multi-target
        # stash.
        if target_path.name.startswith("yagura-daemon"):
            raise OSError("simulated stash failure for yagura-daemon")
        return real_stash_target(target_path, stash_path)

    monkeypatch.setattr(stash_rebuild, "stash_target", flaky_stash_target)

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--bin", "yagura-daemon"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert exit_code != 0
    # cargo build must never start once any target in this run failed to stash.
    assert build_calls == []
    # yagura-mcp was stashed successfully before yagura-daemon failed; it must
    # be restored to its original name rather than left stashed.
    assert original_a.exists()
    assert original_a.read_bytes() == b"binary-a"
    assert list(debug_dir.glob("yagura-mcp.exe.locked-*")) == []
    # yagura-daemon never got stashed in the first place, so it is untouched.
    assert original_b.exists()
    assert original_b.read_bytes() == b"binary-b"


def test_main_invokes_rollback_for_already_stashed_targets_on_partial_stash_failure(
    tmp_path, monkeypatch
):
    # A filesystem-only assertion cannot distinguish "rollback attempted, then
    # skipped because the original name was occupied" from "rollback never
    # attempted at all" -- both leave the same files in place. Track the call
    # to the shared rollback() building block instead, so the reused
    # existing-name condition (tested at the unit level above) is confirmed
    # to actually run for the partial-stash-failure path too, with only the
    # targets stashed so far (not the one that failed) passed in.
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    (debug_dir / "yagura-mcp.exe").write_bytes(b"binary-a")
    (debug_dir / "yagura-daemon.exe").write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 0

    real_stash_target = stash_rebuild.stash_target

    def flaky_stash_target(target_path, stash_path):
        if target_path.name.startswith("yagura-daemon"):
            raise OSError("simulated stash failure for yagura-daemon")
        return real_stash_target(target_path, stash_path)

    monkeypatch.setattr(stash_rebuild, "stash_target", flaky_stash_target)

    rollback_calls = []
    real_rollback = stash_rebuild.rollback

    def tracking_rollback(stashed, originals):
        rollback_calls.append(dict(stashed))
        return real_rollback(stashed, originals)

    monkeypatch.setattr(stash_rebuild, "rollback", tracking_rollback)

    stash_rebuild.main(
        ["--bin", "yagura-mcp", "--bin", "yagura-daemon"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert len(rollback_calls) == 1
    # Only the target actually stashed before the failure is rolled back;
    # yagura-daemon, which never got stashed, is not passed in.
    assert set(rollback_calls[0]) == {"yagura-mcp"}


# ---------------------------------------------------------------------------
# 7. Rollback conditions (main end-to-end)
# ---------------------------------------------------------------------------


def test_main_rolls_back_when_build_fails_and_original_absent(tmp_path):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original = debug_dir / "yagura-mcp.exe"
    original.write_bytes(b"pre-build-binary")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 1

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    assert original.exists()
    assert original.read_bytes() == b"pre-build-binary"
    assert list(debug_dir.glob("yagura-mcp.exe.locked-*")) == []


def test_main_leaves_stash_in_place_when_build_fails_and_original_already_uplifted(tmp_path):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original = debug_dir / "yagura-mcp.exe"
    original.write_bytes(b"pre-build-binary")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        # Simulate cargo uplifting a new binary into place before the build
        # is ultimately reported as failed (e.g. a post-link test failure).
        original.write_bytes(b"uplifted-by-cargo")
        return 1

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    assert original.read_bytes() == b"uplifted-by-cargo"
    remaining_locked = list(debug_dir.glob("yagura-mcp.exe.locked-*"))
    assert len(remaining_locked) == 1


# ---------------------------------------------------------------------------
# 8. Rejecting arguments that change path resolution
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("rejected_arg", ["--target", "--profile"])
def test_main_rejects_extra_args_that_change_path_resolution(tmp_path, rejected_arg):
    build_calls = []

    def fake_metadata(manifest_path):
        return {"target_directory": str(tmp_path / "target")}

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--", rejected_arg, "x86_64-pc-windows-msvc"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert exit_code != 0
    assert build_calls == []


# ---------------------------------------------------------------------------
# 8b. --target-dir among the path-changing cargo arguments
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "rejected_args",
    [
        ["--target-dir", "C:/elsewhere"],
        ["--target-dir=C:/elsewhere"],
    ],
)
def test_main_rejects_target_dir_argument_that_changes_build_output_path(tmp_path, rejected_args):
    build_calls = []

    def fake_metadata(manifest_path):
        return {"target_directory": str(tmp_path / "target")}

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--"] + rejected_args,
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert exit_code != 0
    assert build_calls == []


# ---------------------------------------------------------------------------
# 9. Error paths
# ---------------------------------------------------------------------------


def test_main_propagates_cargo_build_exit_code(tmp_path):
    target_dir = tmp_path / "target"
    (target_dir / "debug").mkdir(parents=True)

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    build_calls = []

    def fake_build(args, cwd):
        build_calls.append(args)
        return 101

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code == 101
    assert len(build_calls) == 1


def test_main_stops_when_cargo_metadata_fails(tmp_path):
    def failing_metadata(manifest_path):
        raise RuntimeError("cargo metadata failed")

    build_calls = []

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=failing_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    assert build_calls == []


def test_main_requires_at_least_one_bin():
    calls = {"metadata": 0, "build": 0}

    def fake_metadata(manifest_path):
        calls["metadata"] += 1
        return {"target_directory": "unused"}

    def fake_build(args, cwd):
        calls["build"] += 1
        return 0

    exit_code = stash_rebuild.main([], metadata_fn=fake_metadata, build_fn=fake_build)

    assert exit_code != 0
    assert calls == {"metadata": 0, "build": 0}


# ---------------------------------------------------------------------------
# 11. --bin name validation
# ---------------------------------------------------------------------------


def test_main_rejects_bin_name_with_parent_traversal_before_touching_any_file(tmp_path):
    target_dir = tmp_path / "target"
    (target_dir / "debug").mkdir(parents=True)
    # Two ".." components from target/debug land exactly here: a real file
    # outside target_directory to prove no rename is attempted against it.
    escape_target = tmp_path / "escape.exe"
    escape_target.write_bytes(b"do-not-touch")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    build_calls = []

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "../../escape"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    assert build_calls == []
    assert escape_target.read_bytes() == b"do-not-touch"


@pytest.mark.parametrize(
    "bin_name",
    [
        "../../escape",
        "sub/escape",
        "sub\\escape",
        "C:/Windows/System32/notepad",
        "C:\\Windows\\System32\\notepad",
    ],
)
def test_main_rejects_bin_names_that_are_not_a_bare_file_name(tmp_path, monkeypatch, bin_name):
    target_dir = tmp_path / "target"
    (target_dir / "debug").mkdir(parents=True)

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    build_calls = []

    def fake_build(args, cwd):
        build_calls.append(args)
        return 0

    # Absolute/drive-letter forms cannot be safely materialized as a real
    # file in this test (they point outside the pytest tmp_path sandbox), so
    # every form is checked the same, OS-safe way: stash_target must never
    # even be called for a rejected name.
    stash_calls = []

    def tracking_stash_target(target_path, stash_path):
        stash_calls.append((target_path, stash_path))
        return False

    monkeypatch.setattr(stash_rebuild, "stash_target", tracking_stash_target)

    exit_code = stash_rebuild.main(
        ["--bin", bin_name], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    assert build_calls == []
    assert stash_calls == []


# ---------------------------------------------------------------------------
# 12. Post-build existence check
#
# A name-based blocklist on cargo_args cannot enumerate every cargo argument
# that redirects the build output path -- `--config build.target-dir=...` has
# the same effect as `--target-dir` but a different argument name. Detecting
# this from the result instead: once the build exits 0, each stashed target
# must have reappeared under its original name, whatever path cargo actually
# used to reach it.
# ---------------------------------------------------------------------------


def test_main_fails_when_stashed_target_is_missing_after_successful_build(tmp_path, capsys):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original = debug_dir / "yagura-mcp.exe"
    original.write_bytes(b"binary")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        # cargo "succeeds" but the stashed target never reappears under its
        # original name, as if its output path had been redirected elsewhere
        # (e.g. `--config build.target-dir=...`).
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    stderr = capsys.readouterr().err
    assert "yagura-mcp" in stderr


def test_main_identifies_which_stashed_target_is_missing_after_successful_build(tmp_path, capsys):
    # With more than one --bin, the check must name the target that did not
    # come back rather than reporting a blanket failure; an implementation
    # that just fails without saying which target is missing would pass the
    # single-target test above but not this one.
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original_a = debug_dir / "yagura-mcp.exe"
    original_a.write_bytes(b"binary-a")
    original_b = debug_dir / "yagura-daemon.exe"
    original_b.write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        # Only yagura-mcp reappears; yagura-daemon's output went elsewhere.
        original_a.write_bytes(b"binary-a-rebuilt")
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--bin", "yagura-daemon"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert exit_code != 0
    stderr = capsys.readouterr().err
    assert "yagura-daemon" in stderr
    assert "yagura-mcp" not in stderr


def test_main_succeeds_when_all_stashed_targets_reappear_after_build(tmp_path):
    # Companion to the two failure tests above: a normal successful build, where
    # every stashed target comes back under its original name, must not be
    # rejected by the check. This is the same shape as
    # test_main_stashes_existing_target_before_build but exercises two
    # targets at once, to confirm the check clears when *all* of them (not
    # just the first) are present.
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original_a = debug_dir / "yagura-mcp.exe"
    original_a.write_bytes(b"binary-a")
    original_b = debug_dir / "yagura-daemon.exe"
    original_b.write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        original_a.write_bytes(b"binary-a-rebuilt")
        original_b.write_bytes(b"binary-b-rebuilt")
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--bin", "yagura-daemon"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert exit_code == 0


# ---------------------------------------------------------------------------
# 13. Error detail and sweep listing errors
#
# Regression contract for _error_detail()'s cargo-stderr inclusion and for
# sweep()'s distinction between "no leftovers yet" (FileNotFoundError) and
# "could not list" (any other OSError). Each case is paired with a
# contrasting case exercising the other branch of the same function, to show
# the assertions are not trivially true regardless of input.
# ---------------------------------------------------------------------------


def test_error_detail_includes_cargo_stderr_from_called_process_error():
    exc = subprocess.CalledProcessError(
        returncode=101,
        cmd=["cargo", "metadata"],
        output="",
        stderr="error: failed to parse manifest at `Cargo.toml`\n\nCaused by:\n  invalid type\n",
    )
    detail = stash_rebuild._error_detail(exc)
    assert "failed to parse manifest" in detail


def test_error_detail_falls_back_to_str_when_called_process_error_has_no_stderr():
    # Contrast with the case above: the same function, on a
    # CalledProcessError without captured stderr, must fall back to str(exc)
    # rather than producing the same cargo-diagnostic text regardless of
    # input.
    exc = subprocess.CalledProcessError(returncode=101, cmd=["cargo", "metadata"], output="", stderr="")
    detail = stash_rebuild._error_detail(exc)
    assert detail == str(exc)
    assert "failed to parse manifest" not in detail


def test_main_error_message_includes_cargo_stderr_on_metadata_failure(tmp_path, capsys):
    def failing_metadata(manifest_path):
        raise subprocess.CalledProcessError(
            returncode=101,
            cmd=["cargo", "metadata"],
            output="",
            stderr="error: failed to parse manifest at `Cargo.toml`\n\nCaused by:\n  invalid type\n",
        )

    def fake_build(args, cwd):
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp"], metadata_fn=failing_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    stderr = capsys.readouterr().err
    assert "failed to parse manifest" in stderr


def test_sweep_reports_list_error_for_non_missing_os_error(tmp_path, monkeypatch):
    directory = tmp_path / "debug"
    directory.mkdir()

    def failing_listdir(path):
        raise PermissionError(13, "Permission denied")

    monkeypatch.setattr(os, "listdir", failing_listdir)

    result = stash_rebuild.sweep(directory, "yagura-mcp", ".exe")

    assert result.list_error is not None
    assert "Permission denied" in result.list_error
    assert result.removed == []
    assert result.remaining == []


def test_sweep_returns_empty_result_without_list_error_when_directory_missing(tmp_path):
    # Contrast with the case above: the same function, given a directory that
    # simply does not exist yet (the pre-first-build case), must report no
    # error at all rather than the "could not list" message.
    missing_dir = tmp_path / "does-not-exist-yet"

    result = stash_rebuild.sweep(missing_dir, "yagura-mcp", ".exe")

    assert result.list_error is None
    assert result.removed == []
    assert result.remaining == []


# ---------------------------------------------------------------------------
# 14. Restoring the stash of targets not touched by a partial build
#
# README §2 names "--bin A --bin B -- -p A" as an unsupported combination that
# is caught from the result: the build succeeds but never produces B. Leaving
# B's stash in place the way a --config target-dir bypass is handled (section
# 12) would be wrong: B's output did not move anywhere, and its original name
# would be left vacated where it was occupied before the run started.
#
# So the stash of every target still missing after a successful build is
# restored to its original name, reusing rollback() and its free-slot
# condition rather than new judgment logic, restored vs. not-restored
# targets are reported separately, and the command still exits non-zero.
# ---------------------------------------------------------------------------


def test_main_restores_stash_of_target_not_rebuilt_in_partial_build(tmp_path):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original_a = debug_dir / "yagura-mcp.exe"
    original_a.write_bytes(b"binary-a")
    original_b = debug_dir / "yagura-daemon.exe"
    original_b.write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        # Only yagura-mcp is passed to cargo via -p; yagura-daemon's output
        # path is never touched by this build (README's "--bin A --bin B --
        # -p A" example).
        original_a.write_bytes(b"binary-a-rebuilt")
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-mcp", "--bin", "yagura-daemon", "--", "-p", "yagura-mcp"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert exit_code != 0
    # yagura-mcp actually was rebuilt; its new binary must be left alone.
    assert original_a.read_bytes() == b"binary-a-rebuilt"
    # yagura-daemon was never part of this build; its stash must be moved
    # back so the original name is not left empty, restoring the state the
    # target was in before this run started.
    assert original_b.exists()
    assert original_b.read_bytes() == b"binary-b"
    assert list(debug_dir.glob("yagura-daemon.exe.locked-*")) == []


def test_main_invokes_rollback_only_for_targets_missing_after_build(tmp_path, monkeypatch):
    # Same rationale as
    # test_main_invokes_rollback_for_already_stashed_targets_on_partial_stash_failure:
    # a filesystem-only assertion cannot distinguish "restored by reusing the
    # shared rollback() building block" from "restored by new, separately
    # written logic that happens to produce the same files". Track the call
    # to confirm that rollback() is reused rather than new judgment logic
    # added, and that only the missing target - not the one that was
    # actually rebuilt - is handed to it.
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    (debug_dir / "yagura-mcp.exe").write_bytes(b"binary-a")
    (debug_dir / "yagura-daemon.exe").write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        (debug_dir / "yagura-mcp.exe").write_bytes(b"binary-a-rebuilt")
        return 0

    rollback_calls = []
    real_rollback = stash_rebuild.rollback

    def tracking_rollback(stashed, originals):
        rollback_calls.append(dict(stashed))
        return real_rollback(stashed, originals)

    monkeypatch.setattr(stash_rebuild, "rollback", tracking_rollback)

    stash_rebuild.main(
        ["--bin", "yagura-mcp", "--bin", "yagura-daemon"],
        metadata_fn=fake_metadata,
        build_fn=fake_build,
    )

    assert len(rollback_calls) == 1
    # yagura-mcp was actually rebuilt: passing it to rollback() would find its
    # original name occupied and misreport a successfully rebuilt target as
    # "left in place" / un-restorable.
    assert set(rollback_calls[0]) == {"yagura-daemon"}


def test_main_reports_restored_missing_target_distinctly(tmp_path, capsys):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original = debug_dir / "yagura-daemon.exe"
    original.write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 0

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-daemon"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    out, err = capsys.readouterr()
    # Exclude the "stashed: <name> -> <path>" line: tmp_path's own directory
    # name embeds a truncated form of this test's name (which happens to
    # contain the substring "restored"), so checking the full text would
    # pass even without the report this test is meant to require.
    report = "\n".join(
        line for line in (out + err).splitlines() if " -> " not in line
    )
    # Reuses the vocabulary _print_summary already uses for a build-failure
    # rollback ("rollback: restored ...") rather than the "NOT restored"
    # wording reserved for a restore that failed (see the companion test
    # below), so a reader can tell the two apart.
    assert "restored" in report
    assert "NOT restored" not in report
    assert "yagura-daemon" in report


def test_main_reports_target_that_could_not_be_restored_separately_from_restored_ones(
    tmp_path, monkeypatch, capsys
):
    target_dir = tmp_path / "target"
    debug_dir = target_dir / "debug"
    debug_dir.mkdir(parents=True)
    original = debug_dir / "yagura-daemon.exe"
    original.write_bytes(b"binary-b")

    def fake_metadata(manifest_path):
        return {"target_directory": str(target_dir)}

    def fake_build(args, cwd):
        return 0

    real_rename = os.rename

    def flaky_rename(src, dst):
        # Fail only the move that would restore a stash back under its
        # original (non-stash) name, simulating a race where the slot became
        # unavailable between the missing-check and the move. The earlier
        # original-name -> stash-name move (stash phase) must keep working.
        moving_stash_back = stash_rebuild.STASH_MARKER in Path(src).name and (
            stash_rebuild.STASH_MARKER not in Path(dst).name
        )
        if moving_stash_back:
            raise OSError("simulated restore failure")
        return real_rename(src, dst)

    monkeypatch.setattr(os, "rename", flaky_rename)

    exit_code = stash_rebuild.main(
        ["--bin", "yagura-daemon"], metadata_fn=fake_metadata, build_fn=fake_build
    )

    assert exit_code != 0
    # The restore attempt failed: the stash must stay where it is rather than
    # being lost, and the original name stays empty.
    assert list(debug_dir.glob("yagura-daemon.exe.locked-*")) != []
    assert not original.exists()
    out, err = capsys.readouterr()
    # Same tmp_path-name-collision concern as the companion test above.
    report = "\n".join(
        line for line in (out + err).splitlines() if " -> " not in line
    )
    assert "NOT restored" in report
    assert "yagura-daemon" in report
