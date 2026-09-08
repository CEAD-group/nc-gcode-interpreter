//! Give `nc-gcode-interpreter --version` a real version in local builds.
//!
//! `Cargo.toml` carries the placeholder version `0.0.0-dev`, which the release
//! CI rewrites from the git tag. A plain `cargo build` in a checkout doesn't go
//! through the PEP 517 shim in `build_backend/` that does the same for wheels,
//! so without this the CLI would report `0.0.0-dev` for every dev build. Derive
//! the same `git describe`-based version here and expose it as
//! `NC_GCODE_INTERPRETER_VERSION`; fall back to the `Cargo.toml` version when
//! there is no git checkout (an unpacked sdist) or the version is already
//! stamped.

use std::process::Command;

const PLACEHOLDER: &str = "0.0.0-dev";

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");

    let cargo_version = env!("CARGO_PKG_VERSION");
    let version = if cargo_version == PLACEHOLDER {
        git_version().unwrap_or_else(|| cargo_version.to_string())
    } else {
        cargo_version.to_string()
    };
    println!("cargo:rustc-env=NC_GCODE_INTERPRETER_VERSION={version}");
}

/// `v0.2.6-3-g1234abc[-dirty]` -> `0.2.6-dev.3+g1234abc[.dirty]`, or the bare
/// tag when we are exactly on a clean tag.
fn git_version() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--long", "--dirty", "--match", "v[0-9]*"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let described = String::from_utf8(output.stdout).ok()?;
    let described = described.trim().strip_prefix('v')?;

    let (described, dirty) = match described.strip_suffix("-dirty") {
        Some(rest) => (rest, true),
        None => (described, false),
    };
    // Split off the trailing `-<distance>-g<sha>`; the tag itself may contain
    // `-`, so scan from the right.
    let (rest, sha) = described.rsplit_once('-')?;
    let (tag, distance) = rest.rsplit_once('-')?;
    let distance: u32 = distance.parse().ok()?;

    if distance == 0 && !dirty {
        return Some(tag.to_string());
    }
    let local = if dirty { format!("{sha}.dirty") } else { sha.to_string() };
    Some(format!("{tag}-dev.{distance}+{local}"))
}
