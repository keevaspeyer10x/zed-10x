#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]
use std::process::Command;

const ZED_MANIFEST: &str = include_str!("../zed/Cargo.toml");

fn main() {
    let zed_cargo_toml: cargo_toml::Manifest =
        toml::from_str(ZED_MANIFEST).expect("failed to parse zed Cargo.toml");
    println!(
        "cargo:rustc-env=ZED_PKG_VERSION={}",
        zed_cargo_toml.package.unwrap().version.unwrap()
    );
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").unwrap()
    );

    // Same runtime stamp as crates/zed/build.rs: option_env! would miss a
    // ZED_BUILD_ID set only for this package build.
    println!("cargo:rerun-if-changed=../../.git/logs/HEAD");
    println!("cargo:rerun-if-env-changed=ZED_COMMIT_SHA");
    println!("cargo:rerun-if-env-changed=ZED_BUILD_ID");
    println!("cargo:rerun-if-env-changed=GITHUB_RUN_NUMBER");
    if let Some(git_sha) = release_commit_sha() {
        println!("cargo:rustc-env=ZED_COMMIT_SHA={git_sha}");
        if let Some(build_identifier) = release_build_id() {
            println!("cargo:rustc-env=ZED_BUILD_ID={build_identifier}");
        }
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn release_commit_sha() -> Option<String> {
    if let Some(git_sha) = nonempty_env("ZED_COMMIT_SHA") {
        if git_sha.len() != 40
            || !git_sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            panic!("ZED_COMMIT_SHA must be 40 lowercase hexadecimal characters");
        }
        return Some(git_sha);
    }

    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let git_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if git_sha.is_empty() {
        None
    } else {
        Some(git_sha)
    }
}

fn release_build_id() -> Option<String> {
    let explicit = nonempty_env("ZED_BUILD_ID");
    let github = nonempty_env("GITHUB_RUN_NUMBER");
    let selected = match (explicit, github) {
        (Some(explicit), Some(github)) if explicit != github => {
            panic!("ZED_BUILD_ID ({explicit}) does not match GITHUB_RUN_NUMBER ({github})");
        }
        (Some(explicit), _) => Some(explicit),
        (None, Some(github)) => Some(github),
        (None, None) => None,
    };
    if let Some(build_id) = &selected
        && !build_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        panic!("release build id must be decimal digits, got {build_id}");
    }
    selected
}
