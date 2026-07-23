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

    const EXIT_TIMEOUT: Duration = Duration::from_secs(12);

    fn wait_with_timeout(
        child: &mut std::process::Child,
    ) -> std::io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + EXIT_TIMEOUT;
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
            remote::STDIN_ENVIRONMENT_CAPABILITY
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
            let mut child = Command::new(env!("CARGO_BIN_EXE_remote_server"))
                .args(["env-exec", "--", "/usr/bin/true"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin.write_all(frame)?;
            let keep_writer_open = case_index == 0;
            if !keep_writer_open {
                drop(stdin);
            }

            let status = wait_with_timeout(&mut child)
                .with_context(|| format!("invalid frame case {case_index}"))?;
            assert!(
                !status.success(),
                "invalid frame executed the requested command: {frame:?}"
            );
        }
        Ok(())
    }
}
