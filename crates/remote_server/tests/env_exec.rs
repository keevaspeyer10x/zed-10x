#[cfg(unix)]
mod unix {
    use anyhow::Context as _;
    use collections::HashMap;
    use std::{
        io::Write as _,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    // The debug remote-server binary is several hundred MiB and can take more
    // than 12 seconds to cold-start on macOS immediately after Cargo relinks
    // it. The product's missing-frame deadline remains five seconds after
    // `main` starts; this outer harness budget only covers process startup.
    const EXIT_TIMEOUT: Duration = Duration::from_secs(45);

    fn wait_with_timeout(
        child: &mut std::process::Child,
    ) -> std::io::Result<std::process::ExitStatus> {
        wait_with_timeout_for(child, EXIT_TIMEOUT)
    }

    fn wait_with_timeout_for(
        child: &mut std::process::Child,
        timeout: Duration,
    ) -> std::io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "env-exec did not exit finitely",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[allow(clippy::disallowed_methods)] // This process-boundary test inspects the real child.
    fn wait_for_command_line(
        child: &mut std::process::Child,
        expected: &str,
    ) -> anyhow::Result<String> {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            let output = Command::new("ps")
                .args(["-p", &child.id().to_string(), "-o", "command="])
                .output()?;
            let command_line = String::from_utf8(output.stdout)?;
            if command_line.contains(expected) {
                return Ok(command_line);
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!(
                    "env-exec exited before command became observable: {status}; \
                     last command line: {command_line}"
                );
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "env-exec command did not become observable; \
                     last command line: {command_line}"
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // This process-boundary test needs the real binary.
    fn env_exec_transfers_environment_without_argv_exposure_and_preserves_stdin()
    -> anyhow::Result<()> {
        let capabilities = Command::new(env!("CARGO_BIN_EXE_remote_server"))
            .arg("capabilities")
            .output()?;
        assert!(capabilities.status.success());
        assert_eq!(
            String::from_utf8(capabilities.stdout)?.trim(),
            format!(
                "{}\n{}",
                remote::STDIN_ENVIRONMENT_CAPABILITY,
                remote::PTY_STDIN_ENVIRONMENT_CAPABILITY,
            )
        );

        let secret = "sentinel-argv-boundary-20260723";
        let suffix = "acp-message";
        let environment = [("PROVIDER_TOKEN".to_string(), secret.to_string())]
            .into_iter()
            .collect::<HashMap<_, _>>();
        let script = "IFS= read -r line; sleep 1; printf '%s|%s' \"$PROVIDER_TOKEN\" \"$line\"";

        let mut child = Command::new(env!("CARGO_BIN_EXE_remote_server"))
            .args(["env-exec", "--", "/bin/sh", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(&remote::encode_stdin_environment(&environment)?)?;
        writeln!(stdin, "{suffix}")?;
        drop(stdin);

        let command_line = wait_for_command_line(&mut child, "IFS= read -r line")?;
        assert!(
            !command_line.contains(secret),
            "environment values must not appear in the live process command line"
        );

        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "env-exec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)?,
            format!("{secret}|{suffix}")
        );
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // This process-boundary test inspects the real child.
    fn env_exec_reaps_a_command_group_that_ignores_transport_eof() -> anyhow::Result<()> {
        use std::io::{BufRead as _, BufReader};

        let script = "trap '' HUP TERM; printf '%s\\n' \"$$\"; while :; do sleep 60; done";
        let mut child = Command::new(env!("CARGO_BIN_EXE_remote_server"))
            .args(["env-exec", "--", "/bin/sh", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(&remote::encode_stdin_environment(&HashMap::default())?)?;
        stdin.flush()?;

        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut pid_line = String::new();
        reader.read_line(&mut pid_line)?;
        let command_pid = pid_line.trim().parse::<u32>()?;

        drop(stdin);
        let status = wait_with_timeout_for(&mut child, Duration::from_secs(5));
        if status.is_err() {
            let _ = Command::new("kill")
                .args(["-KILL", &command_pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let status = status.context("env-exec did not terminate after transport EOF")?;
        assert!(
            !status.success(),
            "transport EOF must terminate an otherwise live command group"
        );

        let still_running = Command::new("kill")
            .args(["-0", &command_pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        assert!(!still_running, "remote command survived env-exec cleanup");
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // This process-boundary test inspects the real child.
    fn env_exec_reaps_descendants_after_the_direct_command_exits() -> anyhow::Result<()> {
        use std::io::{BufRead as _, BufReader};

        let script = "sleep 60 & printf '%s\\n' \"$!\"";
        let mut child = Command::new(env!("CARGO_BIN_EXE_remote_server"))
            .args(["env-exec", "--", "/bin/sh", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(&remote::encode_stdin_environment(&HashMap::default())?)?;
        stdin.flush()?;

        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut pid_line = String::new();
        reader.read_line(&mut pid_line)?;
        let descendant_pid = pid_line.trim().parse::<u32>()?;

        let status = wait_with_timeout_for(&mut child, Duration::from_secs(5));
        if status.is_err() {
            let _ = Command::new("kill")
                .args(["-KILL", &descendant_pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let status = status.context("env-exec did not terminate after its command exited")?;
        assert!(status.success(), "the direct command exited successfully");

        let still_running = Command::new("kill")
            .args(["-0", &descendant_pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        assert!(!still_running, "command descendant survived its owner exit");
        drop(stdin);
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // This process-boundary test needs bounded child polling.
    fn env_exec_rejects_invalid_frames_finitely_before_exec() -> anyhow::Result<()> {
        for (case_index, frame) in [
            b"".as_slice(),
            b"10:{}".as_slice(),
            b"1048577:".as_slice(),
            b"01:{},",
            b"2:{}",
            b"2:{}!",
        ]
        .into_iter()
        .enumerate()
        {
            let mut delayed_writer = None;
            let stdin = if case_index == 0 {
                // A separate process keeps the write side open without sending
                // bytes. This makes the missing-frame timeout deterministic
                // without relying on the test process's pipe lifetime.
                let mut writer = Command::new("/bin/sleep")
                    .arg("30")
                    .stdout(Stdio::piped())
                    .spawn()?;
                let reader = writer.stdout.take().expect("delayed writer stdout");
                delayed_writer = Some(writer);
                Stdio::from(reader)
            } else {
                Stdio::piped()
            };
            let mut child = Command::new(env!("CARGO_BIN_EXE_remote_server"))
                .args(["env-exec", "--", "/usr/bin/true"])
                .stdin(stdin)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;
            if case_index != 0 {
                let mut stdin = child.stdin.take().expect("piped stdin");
                stdin.write_all(frame)?;
                drop(stdin);
            }

            let status = wait_with_timeout(&mut child);
            if let Some(mut writer) = delayed_writer {
                writer.kill()?;
                writer.wait()?;
            }
            let status = match status {
                Ok(status) => status,
                Err(error) => {
                    let mut stderr = String::new();
                    if let Some(mut child_stderr) = child.stderr.take() {
                        use std::io::Read as _;
                        child_stderr.read_to_string(&mut stderr)?;
                    }
                    return Err(error).with_context(|| {
                        format!("invalid frame case {case_index}; child stderr: {stderr}")
                    });
                }
            };
            assert!(
                !status.success(),
                "invalid frame executed the requested command: {frame:?}"
            );
        }
        Ok(())
    }
}
