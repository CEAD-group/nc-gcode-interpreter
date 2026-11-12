"""The in-tree build backend stamps Cargo.toml and restores both Cargo files byte for byte."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

BACKEND = Path(__file__).resolve().parents[2] / "build_backend" / "nc_build_backend.py"


@pytest.fixture()
def backend(tmp_path, monkeypatch):
    pytest.importorskip("maturin")
    spec = importlib.util.spec_from_file_location("_test_nc_build_backend", BACKEND)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "_git_version", lambda root: "0.4.0-dev.1+gabc1234")
    # The backend resolves its root from __file__; point it at the scratch tree.
    (tmp_path / "build_backend").mkdir()
    monkeypatch.setattr(module, "__file__", str(tmp_path / "build_backend" / "x.py"))
    return module


@pytest.mark.parametrize("eol", [b"\n", b"\r\n"], ids=["lf", "crlf"])
def test_stamp_and_byte_exact_restore(backend, tmp_path, eol):
    toml = eol.join([b"[package]", b'name = "x"', b'version = "0.0.0-dev"', b""])
    lock = eol.join([b'name = "x"', b'version = "0.0.0-dev"', b""])
    # Bytes, not text: write_text would emit CRLF on Windows and mask the bug under test.
    (tmp_path / "Cargo.toml").write_bytes(toml)
    (tmp_path / "Cargo.lock").write_bytes(lock)
    with backend._versioned_cargo_toml():
        assert b'version = "0.4.0-dev.1+gabc1234"' + eol in (tmp_path / "Cargo.toml").read_bytes()
        (tmp_path / "Cargo.lock").write_bytes(b"rewritten by cargo")
    assert (tmp_path / "Cargo.toml").read_bytes() == toml
    assert (tmp_path / "Cargo.lock").read_bytes() == lock
