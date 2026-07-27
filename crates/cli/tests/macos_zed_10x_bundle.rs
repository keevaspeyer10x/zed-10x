#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_methods,
    reason = "the integration fixture runs a local fake runtime that exits immediately"
)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path, process::Command};

#[test]
fn built_cli_enters_the_declared_launcher_with_the_isolated_profile() {
    let temp_dir = tempfile::tempdir().unwrap();
    let app_bundle = temp_dir.path().join("Zed 10x.app");
    let contents = app_bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    let fake_home = temp_dir.path().join("home");
    let repository = temp_dir.path().join("repository with spaces");
    let runtime_arguments = temp_dir.path().join("runtime-arguments.txt");
    let runtime_environment = temp_dir.path().join("runtime-environment.txt");
    fs::create_dir_all(&macos).unwrap();
    fs::create_dir_all(&resources).unwrap();
    fs::create_dir_all(&fake_home).unwrap();
    fs::create_dir_all(&repository).unwrap();

    let launcher =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../script/zed-10x-canary-launcher");
    fs::copy(launcher, macos.join("zed-10x-launcher")).unwrap();
    fs::set_permissions(
        macos.join("zed-10x-launcher"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(
        macos.join("zed-10x-runtime"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ZED_10X_TEST_RUNTIME_ARGUMENTS\"\nprintf 'openai=%s\\nanthropic=%s\\napex=%s\\n' \"${OPENAI_API_KEY+set}\" \"${ANTHROPIC_API_KEY+set}\" \"${APEX_BROKER_URL+set}\" > \"$ZED_10X_TEST_RUNTIME_ENVIRONMENT\"\n",
    )
    .unwrap();
    fs::set_permissions(
        macos.join("zed-10x-runtime"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleShortVersionString</key>
    <string>1.13.0-10x</string>
    <key>CFBundleVersion</key>
    <string>20260727.1</string>
    <key>CFBundleExecutable</key>
    <string>zed-10x-launcher</string>
    <key>ZedCliLaunchExecutableDirectly</key>
    <true/>
</dict>
</plist>
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cli"))
        .arg("--zed")
        .arg(&app_bundle)
        .arg("--foreground")
        .arg(&repository)
        .env("HOME", &fake_home)
        .env("ZED_10X_TEST_RUNTIME_ARGUMENTS", &runtime_arguments)
        .env("ZED_10X_TEST_RUNTIME_ENVIRONMENT", &runtime_environment)
        .env("OPENAI_API_KEY", "must-not-reach-runtime")
        .env("ANTHROPIC_API_KEY", "must-not-reach-runtime")
        .env("APEX_BROKER_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let arguments = fs::read_to_string(runtime_arguments)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(arguments[0], "--user-data-dir");
    assert_eq!(
        arguments[1],
        fake_home
            .join("Library/Application Support/Zed 10x")
            .to_string_lossy()
    );
    assert!(
        arguments[2].starts_with("zed-cli://"),
        "the built CLI must hand the launcher its one-shot IPC URL"
    );
    assert_eq!(
        fake_home.join("Library/Application Support/Zed").exists(),
        false,
        "the fixture must never create or mutate normal Zed's profile"
    );
    assert_eq!(
        fs::read_to_string(runtime_environment).unwrap(),
        "openai=\nanthropic=\napex=set\n",
        "direct provider secrets must be stripped while Apex routing remains available"
    );
}
