"""In-tree PEP 517 backend: give local builds a version from the git tag.

`Cargo.toml` carries the placeholder version `0.0.0-dev`, which the release CI
rewrites from the tag before building. Without this shim every non-release
build -- notably an editable `uv sync` in a sibling checkout -- reports
`0.0.0.dev0`, which is useless as a version floor and gives a downstream
consumer no signal that its interpreter is a local edit rather than a release.

So: when the placeholder is still in place and we are inside a git checkout
with tags, rewrite it to a semver dev version derived from `git describe`
(`0.2.6-dev.3+g1234abc`), which maturin normalises to the PEP 440
`0.2.6.dev3+g1234abc`. Any other version in `Cargo.toml` is left alone, so the
CI release path is untouched.
"""

from __future__ import annotations

import re
import subprocess
from contextlib import contextmanager
from pathlib import Path

from maturin import *  # noqa: F401,F403  (re-export maturin's PEP 517 hooks)
from maturin import build_editable as _maturin_build_editable
from maturin import build_sdist as _maturin_build_sdist
from maturin import build_wheel as _maturin_build_wheel
from maturin import prepare_metadata_for_build_editable as _maturin_prepare_metadata_for_build_editable
from maturin import prepare_metadata_for_build_wheel as _maturin_prepare_metadata_for_build_wheel

PLACEHOLDER = "0.0.0-dev"
_VERSION_LINE = re.compile(r'^version = ".*"$', re.MULTILINE)
# v0.2.6-3-g1234abc[-dirty]
_DESCRIBE = re.compile(r"^v(?P<tag>.+)-(?P<distance>\d+)-g(?P<sha>[0-9a-f]+)(?P<dirty>-dirty)?$")


def _git_version(root: Path) -> str | None:
    """Semver dev version from `git describe`, or None if it can't be derived."""
    try:
        described = subprocess.run(
            ["git", "describe", "--tags", "--long", "--dirty", "--match", "v[0-9]*"],
            cwd=root,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    match = _DESCRIBE.match(described)
    if not match:
        return None
    tag, distance, sha = match["tag"], int(match["distance"]), match["sha"]
    if distance == 0 and not match["dirty"]:
        return tag
    local = sha if not match["dirty"] else f"{sha}.dirty"
    return f"{tag}-dev.{distance}+{local}"


@contextmanager
def _versioned_cargo_toml():
    root = Path(__file__).resolve().parent.parent
    cargo_toml = root / "Cargo.toml"
    cargo_lock = root / "Cargo.lock"
    original = cargo_toml.read_text()
    if f'version = "{PLACEHOLDER}"' not in original:
        # CI already stamped the release version; nothing to do.
        yield
        return
    version = _git_version(root)
    if version is None:
        yield
        return
    lock_original = cargo_lock.read_text() if cargo_lock.exists() else None
    cargo_toml.write_text(_VERSION_LINE.sub(f'version = "{version}"', original, count=1))
    try:
        yield
    finally:
        cargo_toml.write_text(original)
        # cargo rewrites the lock to match the patched version during the build.
        if lock_original is not None:
            cargo_lock.write_text(lock_original)


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    with _versioned_cargo_toml():
        return _maturin_build_wheel(wheel_directory, config_settings, metadata_directory)


def build_editable(wheel_directory, config_settings=None, metadata_directory=None):
    with _versioned_cargo_toml():
        return _maturin_build_editable(wheel_directory, config_settings, metadata_directory)


def build_sdist(sdist_directory, config_settings=None):
    with _versioned_cargo_toml():
        return _maturin_build_sdist(sdist_directory, config_settings)


def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None):
    with _versioned_cargo_toml():
        return _maturin_prepare_metadata_for_build_wheel(metadata_directory, config_settings)


def prepare_metadata_for_build_editable(metadata_directory, config_settings=None):
    with _versioned_cargo_toml():
        return _maturin_prepare_metadata_for_build_editable(metadata_directory, config_settings)
