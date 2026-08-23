#![cfg(unix)]

use std::{
    fs::File,
    io::{Read as _, Write as _},
    mem::MaybeUninit,
    os::fd::{AsRawFd as _, FromRawFd as _},
    os::unix::process::CommandExt as _,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use collections::HashMap;
use sha2::{Digest as _, Sha256};

fn open_pty() -> Result<(File, File, libc::termios)> {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: the descriptor pointers reference writable storage and the
    // optional name/termios/winsize inputs are intentionally null.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("creating PTY");
    }
    let mut original = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `slave` is the open PTY descriptor and `original` is writable.
    if unsafe { libc::tcgetattr(slave, original.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("reading initial PTY mode");
    }
    // SAFETY: successful `openpty` returned two newly owned descriptors and
    // successful `tcgetattr` initialized `original`.
    Ok(unsafe {
        (
            File::from_raw_fd(master),
            File::from_raw_fd(slave),
            original.assume_init(),
        )
    })
}

fn read_line_with_timeout(master: &mut File, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!remaining.is_zero(), "timed out reading PTY output");
        let mut descriptor = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: `descriptor` remains valid for the duration of `poll`.
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
            )
        };
        anyhow::ensure!(result > 0, "timed out reading PTY output");
        let mut byte = [0_u8; 1];
        master.read_exact(&mut byte)?;
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(String::from_utf8(bytes)?.trim_end().to_string());
        }
    }
}

#[allow(clippy::disallowed_methods)] // The real-PTY process-boundary test needs synchronous pre-exec descriptor custody.
fn spawn_pty_env_exec(slave: &File, command: &[&str]) -> Result<std::process::Child> {
    let stdin = slave.try_clone()?;
    let stdout = slave.try_clone()?;
    let stderr = slave.try_clone()?;
    let mut process = Command::new(env!("CARGO_BIN_EXE_remote_server"));
    process
        .arg("env-exec-pty")
        .arg("--ready-marker")
        .arg("zed_test_ready")
        .arg("--complete-marker")
        .arg("zed_test_complete")
        .arg("--")
        .args(command)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // SAFETY: this closure runs in the isolated child immediately before exec
    // and uses only async-signal-safe libc calls.
    unsafe {
        process.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    process
        .spawn()
        .context("spawning PTY environment bootstrap")
}

#[test]
fn pty_environment_is_private_and_application_stdin_survives() -> Result<()> {
    let (mut master, slave, original) = open_pty()?;
    let mut child = spawn_pty_env_exec(
        &slave,
        &[
            "/bin/sh",
            "-c",
            "printf '%s' \"$ZED_SENTINEL\" | shasum -a 256; IFS= read -r line; printf 'APP:%s\\n' \"$line\"",
        ],
    )?;
    drop(slave);

    assert_eq!(
        read_line_with_timeout(&mut master, Duration::from_secs(30))?,
        "zed_test_ready"
    );
    let secret = "spaces ' quotes \" unicode-λ and\nnewlines";
    let mut environment = HashMap::default();
    environment.insert("ZED_SENTINEL".to_string(), secret.to_string());
    master.write_all(&remote::encode_stdin_environment(&environment)?)?;
    master.flush()?;
    assert_eq!(
        read_line_with_timeout(&mut master, Duration::from_secs(30))?,
        "zed_test_complete"
    );

    master.write_all(b"application-input\n")?;
    master.flush()?;
    let mut output = String::new();
    for _ in 0..6 {
        let line = read_line_with_timeout(&mut master, Duration::from_secs(5))?;
        output.push_str(&line);
        output.push('\n');
        if line.contains("APP:application-input") {
            break;
        }
    }
    assert!(!output.contains(secret));
    assert!(output.contains(&format!("{:x}", Sha256::digest(secret.as_bytes()))));
    assert!(output.contains("APP:application-input"));
    assert!(child.wait()?.success());

    let mut restored = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: the PTY master remains open and `restored` is writable.
    anyhow::ensure!(
        unsafe { libc::tcgetattr(master.as_raw_fd(), restored.as_mut_ptr()) } == 0,
        "reading restored PTY mode"
    );
    // SAFETY: `tcgetattr` succeeded.
    let restored = unsafe { restored.assume_init() };
    let relevant = libc::ECHO | libc::ECHONL | libc::ICANON;
    assert_eq!(restored.c_lflag & relevant, original.c_lflag & relevant);
    Ok(())
}

#[test]
fn malformed_frame_fails_and_restores_terminal_mode() -> Result<()> {
    let (mut master, slave, original) = open_pty()?;
    let mut child = spawn_pty_env_exec(&slave, &["/bin/true"])?;
    drop(slave);
    assert_eq!(
        read_line_with_timeout(&mut master, Duration::from_secs(30))?,
        "zed_test_ready"
    );
    master.write_all(b"malformed-frame")?;
    master.flush()?;
    assert!(!child.wait()?.success());

    let mut restored = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: the PTY master remains open and `restored` is writable.
    anyhow::ensure!(
        unsafe { libc::tcgetattr(master.as_raw_fd(), restored.as_mut_ptr()) } == 0,
        "reading restored PTY mode after failure"
    );
    // SAFETY: `tcgetattr` succeeded.
    let restored = unsafe { restored.assume_init() };
    let relevant = libc::ECHO | libc::ECHONL | libc::ICANON;
    assert_eq!(restored.c_lflag & relevant, original.c_lflag & relevant);
    Ok(())
}
