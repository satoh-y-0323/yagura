#!/usr/bin/env python3
"""Rebuild binaries that a running process still holds open.

On Windows the linker cannot overwrite an executable that another process has
open, so ``cargo build`` fails while a resident daemon or MCP bridge is
running.  This tool renames the target out of the way first, runs the build,
and sweeps the renamed leftovers on a later run once nothing holds them any
more.  See README.md for the calling conventions, the stash naming rules and
what is out of scope.

Standard library only.  No process is ever terminated, and no external
``rm`` is used: leftovers that cannot be deleted stay where they are and are
reported.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Callable, Dict, List, Mapping, Optional, Sequence

STASH_MARKER = "locked"
LEGACY_MARKER = "old"
TIMESTAMP_FORMAT = "%Y%m%d-%H%M%S"
DEBUG_PROFILE = "debug"
RELEASE_PROFILE = "release"

# Cargo arguments that move the build output away from the stashed path.  They
# are rejected early; _missing_after_build() catches every other way it moves.
PATH_CHANGING_CARGO_ARGS = ("--target", "--profile", "--target-dir")

# A --bin value carrying a separator, a drive letter or ".." would resolve
# outside target_directory once joined into a path; these characters cannot.
_BARE_NAME_PATTERN = re.compile(r"[A-Za-z0-9_.-]+")

EXIT_ERROR = 1
EXIT_USAGE = 2


class UsageError(Exception):
    """Raised for an argument list this tool refuses to act on."""


class _ArgumentParser(argparse.ArgumentParser):
    """Parser that reports errors to the caller instead of exiting the process."""

    def error(self, message: str) -> None:  # type: ignore[override]
        raise UsageError(message)


@dataclass
class SweepResult:
    """Outcome of one sweep: what was deleted and what had to stay."""

    removed: List[Path] = field(default_factory=list)
    remaining: List[Path] = field(default_factory=list)
    list_error: Optional[str] = None


@dataclass
class RollbackOutcome:
    """Outcome of one rollback, by target name."""

    restored: List[str] = field(default_factory=list)
    left_in_place: List[str] = field(default_factory=list)


def generate_stash_name(
    bin_name: str, ext: str, timestamp: datetime, existing_names: Sequence[str] | set[str]
) -> str:
    """Return a stash file name for ``<bin_name><ext>`` that is free in ``existing_names``.

    ``timestamp`` is used as given, so callers pass local time: the name is meant
    to be matched against the modification times shown by a directory listing.
    """
    base = f"{bin_name}{ext}.{STASH_MARKER}-{timestamp.strftime(TIMESTAMP_FORMAT)}"
    taken = set(existing_names)
    if base not in taken:
        return base
    suffix = 2
    while f"{base}-{suffix}" in taken:
        suffix += 1
    return f"{base}-{suffix}"


def is_stash_artifact(filename: str, bin_name: str, ext: str) -> bool:
    """Report whether ``filename`` is a stashed copy of ``<bin_name><ext>``.

    Three forms are recognised, each optionally carrying a ``-<n>`` collision
    suffix: ``<bin><ext>.locked-<timestamp>`` (this tool),
    ``<bin>.locked-<pid><ext>`` (older manual stashes) and
    ``<bin><ext>.old-<timestamp>``.  Normal build products and stashes of other
    targets are not artifacts of ``bin_name``.
    """
    name = re.escape(bin_name)
    suffix = re.escape(ext)
    collision = r"(?:-\d+)?"
    stamp = r"\d{8}-\d{6}"
    forms = (
        rf"{name}{suffix}\.(?:{STASH_MARKER}|{LEGACY_MARKER})-{stamp}{collision}",
        rf"{name}\.{STASH_MARKER}-\d+{collision}{suffix}",
    )
    return any(re.fullmatch(form, filename) is not None for form in forms)


def resolve_target_path(target_directory: Path | str, profile: str, bin_name: str, ext: str) -> Path:
    """Return the build output path of ``<bin_name><ext>`` for ``profile``."""
    return Path(target_directory) / profile / f"{bin_name}{ext}"


def stash_target(target_path: Path, stash_path: Path) -> bool:
    """Move ``target_path`` to ``stash_path``.

    Returns ``True`` when the target was moved and ``False`` when there was
    nothing to move (a first build).  Any other failure - including an already
    occupied ``stash_path`` - is raised as ``OSError`` so that the caller can
    stop before starting a build.
    """
    # os.rename() only refuses an occupied destination on Windows; POSIX
    # renames overwrite it silently, so the check is made explicit here.
    if os.path.exists(stash_path):
        raise FileExistsError(f"stash destination already exists: {stash_path}")
    try:
        os.rename(target_path, stash_path)
    except FileNotFoundError:
        return False
    return True


def sweep(directory: Path | str, bin_name: str, ext: str) -> SweepResult:
    """Delete the stashed copies of ``<bin_name><ext>`` found in ``directory``.

    Deletion uses ``os.remove`` only: an external ``rm`` on Windows moves an
    in-use executable to the recycle bin instead of deleting it.  Anything that
    cannot be deleted - typically a stash a process still holds open - is
    reported in ``remaining`` rather than raised.  A missing ``directory`` (no
    build has happened yet) is not an error and yields an empty result; any
    other listing failure (e.g. a permissions error) is reported through
    ``list_error`` instead of being silently treated as "nothing to sweep".
    """
    result = SweepResult()
    try:
        names = sorted(os.listdir(directory))
    except FileNotFoundError:
        return result
    except OSError as exc:
        result.list_error = f"could not list {directory}: {exc}"
        return result
    for name in names:
        if not is_stash_artifact(name, bin_name, ext):
            continue
        path = Path(directory) / name
        try:
            os.remove(path)
        except OSError:
            result.remaining.append(path)
        else:
            result.removed.append(path)
    return result


def rollback(stashed: Mapping[str, Path], originals: Mapping[str, Path]) -> RollbackOutcome:
    """Move stashed targets back, but only where the original name is free.

    A build can fail after cargo has already uplifted a fresh binary into the
    original name; restoring the stash there would replace that new binary with
    the old one, so those entries are left in place instead.
    """
    outcome = RollbackOutcome()
    for name, stash_path in stashed.items():
        original = originals.get(name)
        # exists() is the POSIX-side defense against overwriting a fresh
        # binary; on Windows os.rename() refuses the occupied name itself.
        if original is None or os.path.exists(original):
            outcome.left_in_place.append(name)
            continue
        try:
            os.rename(stash_path, original)
        except OSError:
            outcome.left_in_place.append(name)
        else:
            outcome.restored.append(name)
    return outcome


def resolve_workspace_manifest() -> Path:
    """Return the workspace manifest, derived from this script's own location.

    Deriving it from the caller's working directory would silently target a
    different workspace when the tool is invoked from another Rust project.
    """
    return Path(__file__).resolve().parents[2] / "Cargo.toml"


def default_exe_suffix() -> str:
    """Return the executable suffix of the running platform."""
    return ".exe" if sys.platform == "win32" else ""


def stashing_supported() -> bool:
    """Report whether stashing is needed on the running platform.

    Only Windows keeps a running executable locked; elsewhere the linker can
    replace it and stashing would be a pointless side effect.
    """
    return sys.platform == "win32"


def run_cargo_metadata(manifest_path: Path) -> Dict[str, object]:
    """Return ``cargo metadata`` for ``manifest_path``, raising on any failure."""
    completed = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
            str(manifest_path),
        ],
        cwd=str(manifest_path.parent),
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(completed.stdout)


def run_cargo_build(cargo_args: Sequence[str], cwd: Path) -> int:
    """Run ``cargo`` with ``cargo_args`` in ``cwd`` and return its exit code."""
    return subprocess.call(["cargo", *cargo_args], cwd=str(cwd))


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = _ArgumentParser(
        prog="stash_rebuild.py",
        description=(
            "Stash the named binaries out of the way, then run cargo build. "
            "See scripts/stash-rebuild/README.md."
        ),
    )
    parser.add_argument(
        "--bin",
        action="append",
        metavar="NAME",
        help="binary to stash before building, without extension (repeatable, at least once)",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="use the release profile instead of debug",
    )
    parser.add_argument(
        "cargo_args",
        nargs="*",
        metavar="-- ARG",
        help="arguments passed on to cargo build after a -- separator",
    )
    args = parser.parse_args(list(argv))
    if not args.bin:
        raise UsageError("at least one --bin NAME is required")
    invalid = [name for name in args.bin if not _is_bare_file_name(name)]
    if invalid:
        raise UsageError(
            f"--bin values must be a plain file name, not a path: {' '.join(invalid)}"
        )
    rejected = [arg for arg in args.cargo_args if _changes_path_resolution(arg)]
    if rejected:
        raise UsageError(
            f"cargo arguments {' '.join(rejected)} change the build output path, "
            f"which would no longer match the stashed target; not supported"
        )
    return args


def _is_bare_file_name(name: str) -> bool:
    """Report whether ``name`` is safe to use as a single path component."""
    if name in ("", ".", ".."):
        return False
    return _BARE_NAME_PATTERN.fullmatch(name) is not None


def _changes_path_resolution(arg: str) -> bool:
    return any(arg == name or arg.startswith(f"{name}=") for name in PATH_CHANGING_CARGO_ARGS)


def _missing_after_build(stashed: Mapping[str, Path], originals: Mapping[str, Path]) -> List[str]:
    """Return the stashed targets that are absent from their original name.

    Checking the result, not the cargo arguments, is what covers every way the
    output path can be moved: ``--config build.target-dir=...`` overrides any
    setting, so no list of argument names can be complete.  A target the build
    simply did not produce is absent too, so the caller restores before failing.
    """
    missing = []
    for name in stashed:
        original = originals.get(name)
        if original is None or not os.path.exists(original):
            missing.append(name)
    return missing


def _existing_names(directory: Path) -> set[str]:
    try:
        return set(os.listdir(directory))
    except OSError:
        return set()


def _oldest_modification(paths: Sequence[Path]) -> Optional[str]:
    stamps = []
    for path in paths:
        try:
            stamps.append(os.path.getmtime(path))
        except OSError:
            continue
    if not stamps:
        return None
    return datetime.fromtimestamp(min(stamps)).strftime("%Y-%m-%d %H:%M:%S")


def _print_summary(
    stashed: Mapping[str, Path],
    swept: Mapping[str, SweepResult],
    rolled_back: Optional[RollbackOutcome],
) -> None:
    print("stash-rebuild summary")
    if stashed:
        for name, path in stashed.items():
            print(f"  stashed: {name} -> {path}")
    else:
        print("  stashed: none")

    for result in swept.values():
        if result.list_error:
            print(f"  {result.list_error}")

    removed = [path for result in swept.values() for path in result.removed]
    remaining = [path for result in swept.values() for path in result.remaining]
    print(f"  recovered stash artifacts: {len(removed)}")
    if remaining:
        oldest = _oldest_modification(remaining)
        print(
            f"  stash artifacts still in use: {len(remaining)}"
            + (f" (oldest: {oldest})" if oldest else "")
        )
        for path in remaining:
            print(f"    {path}")
        print("  they are deleted by a later run once nothing holds them open")

    if rolled_back is None:
        print("  rollback: not needed")
        return
    if not rolled_back.restored and not rolled_back.left_in_place:
        print("  rollback: nothing to restore")
    if rolled_back.restored:
        print(f"  rollback: restored {', '.join(rolled_back.restored)}")
    if rolled_back.left_in_place:
        print(
            f"  rollback: NOT restored {', '.join(rolled_back.left_in_place)} "
            f"(the original name is occupied or the move failed; the stash is kept)"
        )


def _finish(
    stashed: Mapping[str, Path],
    swept: Mapping[str, SweepResult],
    rolled_back: Optional[RollbackOutcome],
    exit_code: int,
    missing: Sequence[str] = (),
) -> int:
    _print_summary(stashed, swept, rolled_back)
    if rolled_back is not None and rolled_back.left_in_place:
        print(
            "error: some stashed targets were not restored (see the summary above)",
            file=sys.stderr,
        )
    if missing:
        print(
            f"error: the build succeeded but {', '.join(missing)} did not reappear under "
            "the original name; either it was not one of the targets this build produces or "
            "the build output went to another path (see the summary above)",
            file=sys.stderr,
        )
    return exit_code


def _stash_phase(
    bin_names: Sequence[str],
    profile_dir: Path,
    ext: str,
    originals: Mapping[str, Path],
) -> tuple[Dict[str, SweepResult], Dict[str, Path], Optional[int]]:
    """Sweep leftovers and stash each of ``bin_names``.

    Returns ``(swept, stashed, early_exit_code)``. ``early_exit_code`` is
    ``None`` when stashing succeeded and the caller should proceed to build;
    otherwise the summary has already been printed and the caller must return
    this code without starting a build.
    """
    swept: Dict[str, SweepResult] = {}
    stashed: Dict[str, Path] = {}
    for name in bin_names:
        swept[name] = sweep(profile_dir, name, ext)
    taken = _existing_names(profile_dir)
    timestamp = datetime.now()
    for name in bin_names:
        stash_name = generate_stash_name(name, ext, timestamp, taken)
        taken.add(stash_name)
        stash_path = profile_dir / stash_name
        try:
            moved = stash_target(originals[name], stash_path)
        except OSError as exc:
            # Undo the stashes made earlier in this run: leaving them stashed
            # would remove those targets from their own names.
            print(f"error: cannot stash {originals[name]}: {exc}", file=sys.stderr)
            print("error: cargo build was not started", file=sys.stderr)
            rolled_back = rollback(stashed, originals) if stashed else None
            return swept, stashed, _finish(stashed, swept, rolled_back, EXIT_ERROR)
        if moved:
            stashed[name] = stash_path
    return swept, stashed, None


def _resolve_target_directory(
    metadata_fn: Callable[[Path], Mapping[str, object]], manifest: Path
) -> Path:
    """Return ``target_directory`` from ``metadata_fn``, raising on any failure."""
    metadata = metadata_fn(manifest)
    return Path(str(metadata["target_directory"]))


def _error_detail(exc: Exception) -> str:
    """Return ``exc``'s message, including cargo's own stderr when present."""
    if isinstance(exc, subprocess.CalledProcessError) and exc.stderr:
        return exc.stderr.strip()
    return str(exc)


def main(
    argv: Sequence[str],
    *,
    metadata_fn: Optional[Callable[[Path], Mapping[str, object]]] = None,
    build_fn: Optional[Callable[[Sequence[str], Path], int]] = None,
) -> int:
    """Run the stash / build / rollback sequence and return the exit code."""
    metadata_fn = metadata_fn or run_cargo_metadata
    build_fn = build_fn or run_cargo_build

    try:
        args = _parse_args(argv)
    except UsageError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_USAGE

    manifest = resolve_workspace_manifest()
    repo_root = manifest.parent
    try:
        target_directory = _resolve_target_directory(metadata_fn, manifest)
    except Exception as exc:  # cargo failed, output unparsable, or key absent
        print(
            f"error: cannot determine the target directory: {_error_detail(exc)}",
            file=sys.stderr,
        )
        return EXIT_ERROR

    profile = RELEASE_PROFILE if args.release else DEBUG_PROFILE
    ext = default_exe_suffix()
    profile_dir = target_directory / profile
    originals = {
        name: resolve_target_path(target_directory, profile, name, ext) for name in args.bin
    }

    swept: Dict[str, SweepResult] = {}
    stashed: Dict[str, Path] = {}
    if stashing_supported():
        swept, stashed, early_exit_code = _stash_phase(args.bin, profile_dir, ext, originals)
        if early_exit_code is not None:
            return early_exit_code

    cargo_args = ["build", "--manifest-path", str(manifest)]
    if args.release:
        cargo_args.append("--release")
    cargo_args.extend(args.cargo_args)
    exit_code = build_fn(cargo_args, repo_root)

    rolled_back = None
    missing: Sequence[str] = ()
    if exit_code != 0 and stashed:
        rolled_back = rollback(stashed, originals)
    elif exit_code == 0:
        missing = _missing_after_build(stashed, originals)
        if missing:
            # A missing target may simply not be one this build produces, so put
            # its stash back before reporting; the rebuilt ones are left alone.
            rolled_back = rollback({name: stashed[name] for name in missing}, originals)
            exit_code = EXIT_ERROR
    return _finish(stashed, swept, rolled_back, exit_code, missing)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
