mod headless_project;

#[cfg(test)]
mod remote_editing_tests;

#[cfg(windows)]
pub mod windows;

pub use headless_project::{HeadlessAppState, HeadlessProject};

use anyhow::{Context as _, Result, anyhow};
use clap::Subcommand;
use client::ProxySettings;
use collections::HashMap;
use extension::ExtensionHostProxy;
use fs::{Fs, RealFs};
use futures::{
    AsyncRead, AsyncWrite, AsyncWriteExt, FutureExt, SinkExt,
    channel::{mpsc, oneshot},
    select, select_biased,
};
use git::GitHostingProviderRegistry;
use gpui::{App, AppContext as _, Context, Entity, UpdateGlobal as _};
use gpui_tokio::Tokio;
use http_client::{Url, read_proxy_from_env};
use language::LanguageRegistry;
use net::async_net::{UnixListener, UnixStream};
use node_runtime::{NodeBinaryOptions, NodeRuntime};
use paths::logs_dir;
use project::{project_settings::ProjectSettings, trusted_worktrees};
use proto::CrashReport;
use release_channel::{AppCommitSha, AppVersion, RELEASE_CHANNEL, ReleaseChannel};
use remote::{
    RemoteClient,
    json_log::LogRecord,
    protocol::{read_message, write_message},
    proxy::{ProxyLaunchError, ProxyMode},
};
use reqwest_client::ReqwestClient;
use rpc::proto::{self, Envelope, REMOTE_SERVER_PROJECT_ID};
use rpc::{AnyProtoClient, TypedEnvelope};
use settings::{Settings, SettingsStore, watch_config_file};
use smol::{
    Timer,
    channel::{Receiver, Sender},
    io::AsyncReadExt,
    stream::StreamExt as _,
};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::Write,
    mem,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use thiserror::Error;
use util::{ResultExt, command::new_command};

#[derive(Subcommand)]
pub enum Commands {
    Run {
        #[arg(long)]
        log_file: PathBuf,
        #[arg(long)]
        pid_file: PathBuf,
        #[arg(long)]
        stdin_socket: PathBuf,
        #[arg(long)]
        stdout_socket: PathBuf,
        #[arg(long)]
        stderr_socket: PathBuf,
    },
    Proxy {
        #[arg(long, conflicts_with = "reconnect_or_start")]
        reconnect: bool,
        #[arg(long, conflicts_with = "reconnect")]
        reconnect_or_start: bool,
        #[arg(long)]
        identifier: String,
    },
    #[command(hide = true)]
    EnvExec {
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<OsString>,
    },
    #[command(hide = true)]
    EnvExecGuardian {
        #[arg(long)]
        owner_fd: i32,
        #[arg(long)]
        report_fd: i32,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<OsString>,
    },
    #[command(hide = true)]
    EnvExecPty {
        #[arg(long)]
        ready_marker: String,
        #[arg(long)]
        complete_marker: String,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<OsString>,
    },
    #[command(hide = true)]
    Capabilities,
    Version,
}

pub fn run(command: Commands) -> anyhow::Result<()> {
    use anyhow::Context;
    use release_channel::{RELEASE_CHANNEL, ReleaseChannel};

    match command {
        Commands::Run {
            log_file,
            pid_file,
            stdin_socket,
            stdout_socket,
            stderr_socket,
        } => execute_run(
            log_file,
            pid_file,
            stdin_socket,
            stdout_socket,
            stderr_socket,
        ),
        Commands::Proxy {
            identifier,
            reconnect,
            reconnect_or_start,
        } => execute_proxy(
            identifier,
            if reconnect {
                ProxyMode::Reconnect
            } else if reconnect_or_start {
                ProxyMode::ReconnectOrStart
            } else {
                ProxyMode::Start
            },
        )
        .context("running proxy on the remote server"),
        Commands::EnvExec { command } => {
            execute_env_exec(command).context("starting command with stdin environment")
        }
        Commands::EnvExecGuardian {
            owner_fd,
            report_fd,
            command,
        } => execute_env_exec_guardian(owner_fd, report_fd, command)
            .context("guarding stdin environment command"),
        Commands::EnvExecPty {
            ready_marker,
            complete_marker,
            command,
        } => execute_env_exec_pty(ready_marker, complete_marker, command)
            .context("starting PTY command with stdin environment"),
        Commands::Capabilities => {
            #[cfg(unix)]
            {
                println!("{}", remote::STDIN_ENVIRONMENT_CAPABILITY);
                println!("{}", remote::PTY_STDIN_ENVIRONMENT_CAPABILITY);
            }
            Ok(())
        }
        Commands::Version => {
            let release_channel = *RELEASE_CHANNEL;
            match release_channel {
                ReleaseChannel::Stable | ReleaseChannel::Preview => {
                    println!("{}", env!("ZED_PKG_VERSION"))
                }
                ReleaseChannel::Nightly | ReleaseChannel::Dev => {
                    let commit_sha =
                        option_env!("ZED_COMMIT_SHA").unwrap_or(release_channel.dev_name());
                    let build_id = option_env!("ZED_BUILD_ID");
                    if let Some(build_id) = build_id {
                        println!("{}+{}", build_id, commit_sha)
                    } else {
                        println!("{commit_sha}");
                    }
                }
            };
            Ok(())
        }
    }
}

#[cfg(unix)]
// This deadline starts only after the remote process has launched. The client
// separately allows 30 seconds for SSH to accept and forward the prelude.
const STDIN_ENVIRONMENT_READ_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(unix)]
/// Directly reads fd 0 before any buffered stdin reader or async runtime starts.
struct UnbufferedStdin {
    deadline: Instant,
}

#[cfg(unix)]
impl std::io::Read for UnbufferedStdin {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        loop {
            let timeout = self.deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out reading remote command environment",
                ));
            }
            let timeout_millis = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
            let mut descriptor = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            };
            // SAFETY: `descriptor` points to one initialized `pollfd` value for
            // the duration of the call.
            let poll_result = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
            if poll_result == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out reading remote command environment",
                ));
            }
            if poll_result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "standard input is not a valid descriptor",
                ));
            }
            if descriptor.revents & libc::POLLERR != 0
                && descriptor.revents & (libc::POLLIN | libc::POLLHUP) == 0
            {
                return Err(std::io::Error::other(
                    "standard input reported a polling error",
                ));
            }

            // SAFETY: `buffer` is valid for `buffer.len()` writable bytes and
            // standard input remains owned by the process across this call.
            let bytes_read =
                unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
            if bytes_read >= 0 {
                return Ok(bytes_read as usize);
            }

            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

#[cfg(unix)]
fn set_close_on_exec(fd: std::os::fd::RawFd, close_on_exec: bool) -> std::io::Result<()> {
    // SAFETY: callers pass an open descriptor owned by this process.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = if close_on_exec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: `fd` remains open and `flags` is an F_GETFD-derived bitset.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // This synchronous boundary owns a real process group.
fn execute_env_exec_guardian(
    owner_fd: i32,
    report_fd: i32,
    command: Vec<OsString>,
) -> anyhow::Result<()> {
    use std::{
        io::Read as _,
        os::{fd::FromRawFd as _, unix::process::CommandExt as _},
        process::{Command, Stdio},
        sync::mpsc,
        thread,
    };

    enum Event {
        OwnerExited(std::io::Result<()>),
        CommandExited(std::io::Result<()>),
    }

    struct ProcessGroupCleanupGuard {
        process_group_id: i32,
        armed: bool,
    }

    impl ProcessGroupCleanupGuard {
        fn new(process_group_id: i32) -> Self {
            Self {
                process_group_id,
                armed: true,
            }
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for ProcessGroupCleanupGuard {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }

            // This guard is armed immediately after spawn, before the provider
            // identity is published. It therefore closes the last orphaning
            // window when report publication or any later guardian setup fails.
            // SAFETY: the guardian created this process group and still owns
            // its child leader. Drop cannot return errors, so cleanup is best
            // effort and bounded by SIGKILL followed by waitpid.
            unsafe {
                libc::killpg(self.process_group_id, libc::SIGKILL);
            }
            loop {
                let result =
                    unsafe { libc::waitpid(self.process_group_id, std::ptr::null_mut(), 0) };
                if result >= 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
        }
    }

    fn wait_without_reaping(pid: i32) -> std::io::Result<()> {
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // Keep the exited leader as a zombie until the owner has killed its
            // process group. This prevents PID/PGID reuse from redirecting the
            // cleanup signal at an unrelated process group.
            // SAFETY: `info` points to writable storage for one `siginfo_t`.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn kill_process_group(process_group_id: i32) -> std::io::Result<()> {
        // SAFETY: the guardian created this process group and retains its
        // unreaped leader until after this call.
        if unsafe { libc::killpg(process_group_id, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    anyhow::ensure!(owner_fd >= 0, "owner liveness descriptor is invalid");
    anyhow::ensure!(report_fd >= 0, "guardian report descriptor is invalid");
    let (program, arguments) = command
        .split_first()
        .context("stdin environment command is missing")?;

    set_close_on_exec(owner_fd, true).context("protecting owner liveness descriptor")?;
    // SAFETY: this hidden command receives exclusive ownership of `owner_fd`
    // from its parent and reconstructs exactly one File for it.
    let mut owner = unsafe { File::from_raw_fd(owner_fd) };
    set_close_on_exec(report_fd, true).context("protecting guardian report descriptor")?;
    // SAFETY: this hidden command also receives exclusive ownership of
    // `report_fd` from its parent.
    let mut report = unsafe { File::from_raw_fd(report_fd) };

    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(debug_assertions)]
    command.env_remove("ZED_ENV_EXEC_TEST_GUARDIAN_REPORT_DELAY_MS");
    // The provider gets its own checked session and process group. Every
    // ordinary worker it spawns inherits that group unless it deliberately
    // daemonizes, which is outside the ACP provider contract.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .context("executing guarded stdin environment command")?;
    let child_pid = child.id() as i32;
    let mut cleanup_guard = ProcessGroupCleanupGuard::new(child_pid);
    #[cfg(debug_assertions)]
    if let Ok(delay_ms) = std::env::var("ZED_ENV_EXEC_TEST_GUARDIAN_REPORT_DELAY_MS") {
        let delay_ms = delay_ms
            .parse::<u64>()
            .context("parsing guardian report test delay")?;
        thread::sleep(Duration::from_millis(delay_ms));
    }
    writeln!(report, "{child_pid}").context("reporting guarded command identity")?;
    report
        .flush()
        .context("publishing guarded command identity")?;

    let (event_tx, event_rx) = mpsc::channel();
    let owner_tx = event_tx.clone();
    thread::spawn(move || {
        let result = loop {
            let mut buffer = [0_u8; 1];
            match owner.read(&mut buffer) {
                Ok(0) => break Ok(()),
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        owner_tx.send(Event::OwnerExited(result)).ok();
    });
    thread::spawn(move || {
        event_tx
            .send(Event::CommandExited(wait_without_reaping(child_pid)))
            .ok();
    });

    match event_rx
        .recv()
        .context("waiting for guarded stdin environment command")?
    {
        Event::OwnerExited(result) => {
            result.context("watching stdin environment command owner")?;
            kill_process_group(child_pid).context("terminating orphaned command group")?;
            child.wait().context("reaping orphaned command leader")?;
            cleanup_guard.disarm();
            writeln!(report, "clean").ok();
            report.flush().ok();
            anyhow::bail!("stdin environment command owner exited")
        }
        Event::CommandExited(result) => {
            result.context("observing stdin environment command exit")?;
            if let Err(error) = kill_process_group(child_pid)
                // Darwin reports EPERM when the process group contains only
                // its unreaped zombie leader. A group with a live same-user
                // descendant remains signalable; the descendant regression
                // below exercises that distinct case.
                && error.raw_os_error() != Some(libc::EPERM)
            {
                return Err(error).context("reaping remaining command group");
            }
            let status = child.wait().context("reaping stdin environment command")?;
            cleanup_guard.disarm();
            writeln!(report, "clean").ok();
            report.flush().ok();
            if status.success() {
                Ok(())
            } else {
                anyhow::bail!("stdin environment command exited with {status}")
            }
        }
    }
}

#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // This synchronous process boundary proxies stdio threads.
fn execute_env_exec(command: Vec<OsString>) -> anyhow::Result<()> {
    use std::{
        io::{BufRead as _, BufReader, Read as _, Write as _},
        os::{fd::AsRawFd as _, unix::net::UnixStream, unix::process::CommandExt as _},
        process::{Command, ExitStatus, Stdio},
        sync::mpsc,
        thread,
    };

    enum Event {
        TransportClosed(std::io::Result<u64>),
        ChildExited(std::io::Result<ExitStatus>),
    }

    const TRANSPORT_CLOSE_GRACE: Duration = Duration::from_secs(2);

    anyhow::ensure!(!command.is_empty(), "stdin environment command is missing");
    let mut stdin = UnbufferedStdin {
        deadline: Instant::now() + STDIN_ENVIRONMENT_READ_TIMEOUT,
    };
    let environment =
        remote::read_stdin_environment(&mut stdin).context("reading stdin environment frame")?;

    let current_exe = std::env::current_exe().context("resolving env-exec guardian")?;
    let (owner_reader, owner_writer) =
        UnixStream::pair().context("creating env-exec owner liveness pipe")?;
    let (report_reader, report_writer) =
        UnixStream::pair().context("creating env-exec guardian report pipe")?;
    let owner_fd = owner_reader.as_raw_fd();
    let report_fd = report_writer.as_raw_fd();
    set_close_on_exec(owner_fd, true).context("protecting env-exec liveness reader")?;
    set_close_on_exec(owner_writer.as_raw_fd(), true)
        .context("protecting env-exec liveness writer")?;
    set_close_on_exec(report_reader.as_raw_fd(), true)
        .context("protecting env-exec guardian report reader")?;
    set_close_on_exec(report_fd, true).context("protecting env-exec guardian report writer")?;
    let mut guardian = Command::new(current_exe);
    guardian
        .arg("env-exec-guardian")
        .args([
            "--owner-fd",
            &owner_fd.to_string(),
            "--report-fd",
            &report_fd.to_string(),
            "--",
        ])
        .args(command)
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The child must retain only the liveness reader across exec. The writer
    // remains exclusively owned by this supervisor, so SIGKILL closes it and
    // wakes the guardian without any catchable-signal dependency.
    unsafe {
        guardian.pre_exec(move || {
            let flags = libc::fcntl(owner_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(owner_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(report_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(report_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = guardian
        .spawn()
        .context("executing stdin environment command guardian")?;
    drop(owner_reader);
    drop(report_writer);
    let mut owner_writer = Some(owner_writer);
    report_reader
        .set_read_timeout(Some(Duration::from_secs(30)))
        .context("bounding guardian identity report")?;
    let mut guardian_report = BufReader::new(report_reader);
    let mut command_pid_line = String::new();
    guardian_report
        .read_line(&mut command_pid_line)
        .context("reading guarded command identity")?;
    let command_process_group = command_pid_line
        .trim()
        .parse::<i32>()
        .context("parsing guarded command identity")?;
    anyhow::ensure!(
        command_process_group > 0,
        "guarded command identity is invalid"
    );
    let mut child_stdin = child.stdin.take().context("taking command stdin")?;
    let mut child_stdout = child.stdout.take().context("taking command stdout")?;
    let mut child_stderr = child.stderr.take().context("taking command stderr")?;

    let (event_tx, event_rx) = mpsc::channel();
    let transport_tx = event_tx.clone();
    thread::spawn(move || {
        let result = std::io::copy(&mut std::io::stdin().lock(), &mut child_stdin);
        drop(child_stdin);
        transport_tx.send(Event::TransportClosed(result)).ok();
    });

    let stdout_thread = thread::spawn(move || -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        std::io::copy(&mut child_stdout, &mut stdout)?;
        stdout.flush()
    });
    let stderr_thread = thread::spawn(move || -> std::io::Result<()> {
        let mut stderr = std::io::stderr().lock();
        std::io::copy(&mut child_stderr, &mut stderr)?;
        stderr.flush()
    });

    thread::spawn(move || {
        event_tx.send(Event::ChildExited(child.wait())).ok();
    });

    let mut transport_closed = false;
    let mut transport_error = None;
    let status = loop {
        match event_rx
            .recv()
            .context("waiting for stdin environment command")?
        {
            Event::ChildExited(status) => {
                let status = status.context("waiting for command exit")?;
                let mut cleanup_report = String::new();
                let cleanup_reported = guardian_report.read_to_string(&mut cleanup_report).is_ok()
                    && cleanup_report.lines().any(|line| line == "clean");
                if !cleanup_reported {
                    // A guardian crash must not orphan the provider's separate
                    // process group. The exact PGID was published before this
                    // supervisor accepted the launch.
                    let result = unsafe { libc::killpg(command_process_group, libc::SIGKILL) };
                    if result != 0 {
                        let error = std::io::Error::last_os_error();
                        let already_gone = error.raw_os_error() == Some(libc::ESRCH)
                            || cfg!(target_os = "macos")
                                && error.raw_os_error() == Some(libc::EPERM);
                        if !already_gone {
                            return Err(error)
                                .context("terminating command group after guardian exit");
                        }
                    }
                }
                break status;
            }
            Event::TransportClosed(result) => {
                transport_closed = true;
                if let Err(error) = result {
                    transport_error = Some(error);
                }

                match event_rx.recv_timeout(TRANSPORT_CLOSE_GRACE) {
                    Ok(Event::ChildExited(status)) => {
                        let status = status.context("waiting for command exit")?;
                        break status;
                    }
                    Ok(Event::TransportClosed(_)) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Closing the sole writer tells the guardian that its
                        // transport owner is gone. The same EOF is generated by
                        // the kernel if this supervisor is killed abruptly.
                        drop(owner_writer.take());
                        // Continue the outer loop: the supervisor must not
                        // return until the guardian reports ChildExited after
                        // killing and reaping the owned command group.
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("stdin environment command monitor disconnected")
                    }
                }
            }
        }
    };

    stdout_thread
        .join()
        .map_err(|_| anyhow!("command stdout forwarding thread panicked"))?
        .context("forwarding command stdout")?;
    stderr_thread
        .join()
        .map_err(|_| anyhow!("command stderr forwarding thread panicked"))?
        .context("forwarding command stderr")?;

    if let Some(error) = transport_error {
        return Err(error).context("forwarding command stdin");
    }

    if status.success() {
        Ok(())
    } else if transport_closed {
        anyhow::bail!("stdin environment command terminated after transport closed: {status}")
    } else {
        anyhow::bail!("stdin environment command exited with {status}")
    }
}

#[cfg(unix)]
struct TerminalModeGuard {
    original: libc::termios,
    restored: bool,
}

#[cfg(unix)]
impl TerminalModeGuard {
    fn enter_private_input_mode() -> anyhow::Result<Self> {
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `original` points to writable storage for one `termios` value.
        let result = unsafe { libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("reading terminal mode");
        }
        // SAFETY: `tcgetattr` succeeded and initialized `original`.
        let original = unsafe { original.assume_init() };
        let mut private = original;
        private.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON);
        private.c_cc[libc::VMIN] = 1;
        private.c_cc[libc::VTIME] = 0;
        // SAFETY: stdin is a terminal (proved by `tcgetattr`) and `private`
        // remains valid for the duration of the call.
        let result = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &private) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("setting private terminal mode");
        }
        Ok(Self {
            original,
            restored: false,
        })
    }

    fn restore(&mut self) -> anyhow::Result<()> {
        if self.restored {
            return Ok(());
        }
        // SAFETY: `original` was returned by `tcgetattr` for this descriptor.
        let result = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("restoring terminal mode");
        }
        self.restored = true;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if !self.restored {
            // Best effort only in `Drop`; the main path propagates restoration
            // errors before executing the requested command.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
            }
        }
    }
}

#[cfg(unix)]
fn validate_pty_marker(marker: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !marker.is_empty()
            && marker.len() <= 128
            && marker
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid PTY environment marker"
    );
    Ok(())
}

#[cfg(unix)]
fn execute_env_exec_pty(
    ready_marker: String,
    complete_marker: String,
    command: Vec<OsString>,
) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt as _;

    validate_pty_marker(&ready_marker)?;
    validate_pty_marker(&complete_marker)?;
    anyhow::ensure!(ready_marker != complete_marker, "PTY markers must differ");
    let (program, arguments) = command
        .split_first()
        .context("stdin environment command is missing")?;

    let mut terminal_mode = TerminalModeGuard::enter_private_input_mode()?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{ready_marker}")?;
    stdout.flush()?;

    let mut stdin = UnbufferedStdin {
        deadline: Instant::now() + STDIN_ENVIRONMENT_READ_TIMEOUT,
    };
    let environment =
        remote::read_stdin_environment(&mut stdin).context("reading PTY environment frame")?;
    terminal_mode.restore()?;
    writeln!(stdout, "{complete_marker}")?;
    stdout.flush()?;
    drop(stdout);

    let error = std::process::Command::new(program)
        .args(arguments)
        .envs(environment)
        .exec();
    Err(error).context("executing stdin environment PTY command")
}

#[cfg(not(unix))]
fn execute_env_exec(_command: Vec<OsString>) -> anyhow::Result<()> {
    anyhow::bail!("stdin environment command is not supported on this platform")
}

#[cfg(not(unix))]
fn execute_env_exec_pty(
    _ready_marker: String,
    _complete_marker: String,
    _command: Vec<OsString>,
) -> anyhow::Result<()> {
    anyhow::bail!("PTY stdin environment command is not supported on this platform")
}

pub static VERSION: LazyLock<String> = LazyLock::new(|| match *RELEASE_CHANNEL {
    ReleaseChannel::Stable | ReleaseChannel::Preview => env!("ZED_PKG_VERSION").to_owned(),
    ReleaseChannel::Nightly | ReleaseChannel::Dev => {
        let commit_sha = option_env!("ZED_COMMIT_SHA").unwrap_or("missing-zed-commit-sha");
        let build_identifier = option_env!("ZED_BUILD_ID");
        if let Some(build_id) = build_identifier {
            format!("{build_id}+{commit_sha}")
        } else {
            commit_sha.to_owned()
        }
    }
});

fn init_logging_proxy() {
    env_logger::builder()
        .format(|buf, record| {
            let mut log_record = LogRecord::new(record);
            log_record.message =
                std::borrow::Cow::Owned(format!("(remote proxy) {}", log_record.message));
            serde_json::to_writer(&mut *buf, &log_record)?;
            buf.write_all(b"\n")?;
            Ok(())
        })
        .init();
}

const REMOTE_SERVER_LOG_MAX_BYTES: u64 = 1024 * 1024;

struct RotatingLogFile {
    path: PathBuf,
    file: File,
    size_bytes: u64,
}

impl RotatingLogFile {
    fn open(path: &Path) -> Result<Self> {
        if std::fs::metadata(path)
            .map(|metadata| metadata.len() >= REMOTE_SERVER_LOG_MAX_BYTES)
            .unwrap_or(false)
        {
            rotate_log_file(path, &rotated_log_path(path))
                .context("failed to rotate existing remote server log")?;
        }

        let file = open_log_file(path).context("failed to open remote server log")?;
        let size_bytes = file
            .metadata()
            .context("failed to read remote server log metadata")?
            .len();

        Ok(Self {
            path: path.to_path_buf(),
            file,
            size_bytes,
        })
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        rotate_log_file(&self.path, &rotated_log_path(&self.path))?;
        self.file = open_log_file(&self.path)?;
        self.size_bytes = 0;
        Ok(())
    }
}

impl Write for RotatingLogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.size_bytes.saturating_add(buf.len() as u64) > REMOTE_SERVER_LOG_MAX_BYTES {
            self.rotate()?;
        }

        self.file.write_all(buf)?;
        self.size_bytes += buf.len() as u64;

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn open_log_file(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

fn rotated_log_path(path: &Path) -> PathBuf {
    path.with_extension("1.log")
}

fn rotate_log_file(path: &Path, rotated_path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(rotated_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    match std::fs::rename(path, rotated_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    Ok(())
}

fn init_logging_server(log_file_path: &Path) -> Result<Receiver<Vec<u8>>> {
    struct MultiWrite {
        file: RotatingLogFile,
        channel: Sender<Vec<u8>>,
        buffer: Vec<u8>,
    }

    impl Write for MultiWrite {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.file.write_all(buf)?;
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.channel
                .send_blocking(self.buffer.clone())
                .map_err(std::io::Error::other)?;
            self.buffer.clear();
            self.file.flush()
        }
    }

    let log_file = RotatingLogFile::open(log_file_path)
        .context("Failed to open rotating remote server log file")?;

    let (tx, rx) = async_channel::unbounded();

    let target = Box::new(MultiWrite {
        file: log_file,
        channel: tx,
        buffer: Vec::new(),
    });

    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let message = info.payload_as_str().unwrap_or("Box<Any>").to_owned();
        let location = info
            .location()
            .map_or_else(|| "<unknown>".to_owned(), |location| location.to_string());
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("<unnamed>");

        let msg = format!("thread '{thread_name}' panicked at {location}:\n{message}\n{backtrace}");
        // NOTE: This log never reaches the client, as the communication is handled on a main thread task
        // which will never run once we panic.
        log::error!("{msg}");
        old_hook(info);
    }));
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .target(env_logger::Target::Pipe(target))
        .format(|buf, record| {
            let mut log_record = LogRecord::new(record);
            log_record.message =
                std::borrow::Cow::Owned(format!("(remote server) {}", log_record.message));
            serde_json::to_writer(&mut *buf, &log_record)?;
            buf.write_all(b"\n")?;
            Ok(())
        })
        .init();

    Ok(rx)
}

/// Initializes the telemetry queue on the remote server, forwarding every
/// emitted event to the connected client over the proto channel.
///
/// The remote server cannot upload telemetry itself (it has no logged-in user,
/// no checksum seed, and no Telemetry instance), so without this its
/// `telemetry::event!` calls are silently dropped. The client attributes these
/// events to the remote host using the platform it already detected during
/// connection setup, so no OS metadata needs to be sent here.
fn init_telemetry_forwarding(session: AnyProtoClient, cx: &mut App) {
    let (tx, mut rx) = mpsc::unbounded::<telemetry::Event>();
    telemetry::init(tx);

    cx.background_spawn(async move {
        while let Some(event) = rx.next().await {
            let Some(event_json) = serde_json::to_string(&event).log_err() else {
                continue;
            };
            session
                .send(proto::TelemetryEvent {
                    project_id: REMOTE_SERVER_PROJECT_ID,
                    event_json,
                })
                .log_err();
        }
    })
    .detach();
}

fn handle_crash_files_requests(project: &Entity<HeadlessProject>, client: &AnyProtoClient) {
    client.add_request_handler(
        project.downgrade(),
        |_, _: TypedEnvelope<proto::GetCrashFiles>, _cx| async move {
            let mut legacy_panics = Vec::new();
            let mut crashes = Vec::new();
            let mut children = smol::fs::read_dir(paths::logs_dir()).await?;
            while let Some(child) = children.next().await {
                let child = child?;
                let child_path = child.path();

                let extension = child_path.extension();
                if extension == Some(OsStr::new("panic")) {
                    let filename = if let Some(filename) = child_path.file_name() {
                        filename.to_string_lossy()
                    } else {
                        continue;
                    };

                    if !filename.starts_with("zed") {
                        continue;
                    }

                    let file_contents = smol::fs::read_to_string(&child_path)
                        .await
                        .context("error reading panic file")?;

                    legacy_panics.push(file_contents);
                    smol::fs::remove_file(&child_path)
                        .await
                        .context("error removing panic")
                        .log_err();
                } else if extension == Some(OsStr::new("dmp")) {
                    let mut json_path = child_path.clone();
                    json_path.set_extension("json");
                    if let Ok(json_content) = smol::fs::read_to_string(&json_path).await {
                        crashes.push(CrashReport {
                            metadata: json_content,
                            minidump_contents: smol::fs::read(&child_path).await?,
                        });
                        smol::fs::remove_file(&child_path).await.log_err();
                        smol::fs::remove_file(&json_path).await.log_err();
                    } else {
                        log::error!("Couldn't find json metadata for crash: {child_path:?}");
                    }
                }
            }

            anyhow::Ok(proto::GetCrashFilesResponse { crashes })
        },
    );
}

struct ServerListeners {
    stdin: UnixListener,
    stdout: UnixListener,
    stderr: UnixListener,
}

impl ServerListeners {
    pub fn new(stdin_path: PathBuf, stdout_path: PathBuf, stderr_path: PathBuf) -> Result<Self> {
        Ok(Self {
            stdin: UnixListener::bind(stdin_path).context("failed to bind stdin socket")?,
            stdout: UnixListener::bind(stdout_path).context("failed to bind stdout socket")?,
            stderr: UnixListener::bind(stderr_path).context("failed to bind stderr socket")?,
        })
    }
}

fn start_server(
    listeners: ServerListeners,
    log_rx: Receiver<Vec<u8>>,
    cx: &mut App,
    is_wsl_interop: bool,
) -> AnyProtoClient {
    // This is the server idle timeout. If no connection comes in this timeout, the server will shut down.
    const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

    let (incoming_tx, incoming_rx) = mpsc::unbounded::<Envelope>();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded::<Envelope>();
    let (app_quit_tx, mut app_quit_rx) = mpsc::unbounded::<()>();

    cx.on_app_quit(move |_| {
        let mut app_quit_tx = app_quit_tx.clone();
        async move {
            log::info!("app quitting. sending signal to server main loop");
            app_quit_tx.send(()).await.ok();
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        loop {
            let streams = futures::future::join3(
                listeners.stdin.accept(),
                listeners.stdout.accept(),
                listeners.stderr.accept(),
            );

            log::info!("accepting new connections");
            let result = select! {
                streams = streams.fuse() => {
                    let (Ok((stdin_stream, _)), Ok((stdout_stream, _)), Ok((stderr_stream, _))) = streams else {
                        log::error!("failed to accept new connections");
                        break;
                    };
                    log::info!("accepted new connections");
                    anyhow::Ok((stdin_stream, stdout_stream, stderr_stream))
                }
                _ = futures::FutureExt::fuse(cx.background_executor().timer(IDLE_TIMEOUT)) => {
                    log::warn!("timed out waiting for new connections after {:?}. exiting.", IDLE_TIMEOUT);
                    cx.update(|cx| {
                        // TODO: This is a hack, because in a headless project, shutdown isn't executed
                        // when calling quit, but it should be.
                        cx.shutdown();
                        cx.quit();
                    });
                    break;
                }
                _ = app_quit_rx.next().fuse() => {
                    log::info!("app quit requested");
                    break;
                }
            };

            let Ok((mut stdin_stream, mut stdout_stream, mut stderr_stream)) = result else {
                break;
            };

            let mut input_buffer = Vec::new();
            let mut output_buffer = Vec::new();

            let (mut stdin_msg_tx, mut stdin_msg_rx) = mpsc::unbounded::<Envelope>();
            cx.background_spawn(async move {
                loop {
                    match read_message(&mut stdin_stream, &mut input_buffer).await {
                        Ok(msg) => {
                            if (stdin_msg_tx.send(msg).await).is_err() {
                                log::info!("stdin message channel closed, stopping stdin reader");
                                break;
                            }
                        }
                        Err(error) => {
                            log::warn!("stdin read failed: {error:?}");
                            break;
                        }
                    }
                }
            }).detach();

            loop {

                select_biased! {
                    _ = app_quit_rx.next().fuse() => {
                        return anyhow::Ok(());
                    }

                    stdin_message = stdin_msg_rx.next().fuse() => {
                        let Some(message) = stdin_message else {
                            log::warn!("error reading message on stdin, dropping connection.");
                            break;
                        };
                        if let Err(error) = incoming_tx.unbounded_send(message) {
                            log::error!("failed to send message to application: {error:?}. exiting.");
                            return Err(anyhow!(error));
                        }
                    }

                    outgoing_message  = outgoing_rx.next().fuse() => {
                        let Some(message) = outgoing_message else {
                            log::error!("stdout handler, no message");
                            break;
                        };

                        if let Err(error) =
                            write_message(&mut stdout_stream, &mut output_buffer, message).await
                        {
                            log::error!("failed to write stdout message: {:?}", error);
                            break;
                        }
                        if let Err(error) = stdout_stream.flush().await {
                            log::error!("failed to flush stdout message: {:?}", error);
                            break;
                        }
                    }

                    log_message = log_rx.recv().fuse() => {
                        if let Ok(log_message) = log_message {
                            if let Err(error) = stderr_stream.write_all(&log_message).await {
                                log::error!("failed to write log message to stderr: {:?}", error);
                                break;
                            }
                            if let Err(error) = stderr_stream.flush().await {
                                log::error!("failed to flush stderr stream: {:?}", error);
                                break;
                            }
                        }
                    }
                }
            }
        }
        anyhow::Ok(())
    })
    .detach();

    RemoteClient::proto_client_from_channels(incoming_rx, outgoing_tx, cx, "server", is_wsl_interop)
}

fn init_paths() -> anyhow::Result<()> {
    for path in [
        paths::config_dir(),
        paths::extensions_dir(),
        paths::languages_dir(),
        paths::logs_dir(),
        paths::temp_dir(),
        paths::hang_traces_dir(),
        paths::remote_extensions_dir(),
        paths::remote_extensions_uploads_dir(),
    ]
    .iter()
    {
        std::fs::create_dir_all(path).with_context(|| format!("creating directory {path:?}"))?;
    }
    Ok(())
}

pub fn execute_run(
    log_file: PathBuf,
    pid_file: PathBuf,
    stdin_socket: PathBuf,
    stdout_socket: PathBuf,
    stderr_socket: PathBuf,
) -> Result<()> {
    init_paths()?;

    let startup_time = Instant::now();
    let app = gpui_platform::headless();
    let pid = std::process::id();
    let id = pid.to_string();
    let should_install_crash_handler =
        client::telemetry::should_install_crash_handler(*RELEASE_CHANNEL);

    let crash_handler = if should_install_crash_handler {
        Some(app.background_executor().spawn(crashes::init(
            crashes::InitCrashHandler {
                session_id: id,
                zed_version: VERSION.to_owned(),
                binary: "zed-remote-server".to_string(),
                release_channel: release_channel::RELEASE_CHANNEL_NAME.clone(),
                commit_sha: option_env!("ZED_COMMIT_SHA").unwrap_or("no_sha").to_owned(),
            },
            {
                let background_executor = app.background_executor();
                move |task| {
                    background_executor.spawn(task).detach();
                }
            },
            |pid| paths::temp_dir().join(format!("zed-remote-server-crash-handler-{pid}")),
            // we are running outside gpui
            #[allow(clippy::disallowed_methods)]
            |duration| FutureExt::map(Timer::after(duration), |_| ()),
        )))
    } else {
        crashes::force_backtrace();
        None
    };
    let log_rx = init_logging_server(&log_file)?;
    log::info!(
        "starting up with PID {}:\npid_file: {:?}, log_file: {:?}, stdin_socket: {:?}, stdout_socket: {:?}, stderr_socket: {:?}",
        pid,
        pid_file,
        log_file,
        stdin_socket,
        stdout_socket,
        stderr_socket
    );

    write_pid_file(&pid_file, pid)
        .with_context(|| format!("failed to write pid file: {:?}", &pid_file))?;

    let listeners = ServerListeners::new(stdin_socket, stdout_socket, stderr_socket)?;

    rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(1, |n| n.get().div_ceil(2)))
        .stack_size(10 * 1024 * 1024)
        .thread_name(|ix| format!("RayonWorker{}", ix))
        .build_global()
        .unwrap();

    #[cfg(unix)]
    let shell_env_loaded_rx = {
        let (shell_env_loaded_tx, shell_env_loaded_rx) = oneshot::channel();
        app.background_executor()
            .spawn(async {
                util::load_login_shell_environment().await.log_err();
                shell_env_loaded_tx.send(()).ok();
            })
            .detach();
        Some(shell_env_loaded_rx)
    };
    #[cfg(windows)]
    let shell_env_loaded_rx: Option<oneshot::Receiver<()>> = None;

    let git_hosting_provider_registry = Arc::new(GitHostingProviderRegistry::new());
    let run = move |cx: &mut App| {
        if let Some(crash_handler) = crash_handler {
            cx.spawn(async move |_cx| {
                let _crash_handler = crash_handler.await;
                // cx.update(|cx| cx.set_global(CrashHandler(crash_handler)))
            })
            .detach();
        }
        settings::init(cx);
        let app_commit_sha = option_env!("ZED_COMMIT_SHA").map(|s| AppCommitSha::new(s.to_owned()));
        let app_version = AppVersion::load(
            env!("ZED_PKG_VERSION"),
            option_env!("ZED_BUILD_ID"),
            app_commit_sha,
        );
        release_channel::init(app_version, cx);
        gpui_tokio::init(cx);

        HeadlessProject::init(cx);

        let is_wsl_interop = if cfg!(target_os = "linux") {
            // See: https://learn.microsoft.com/en-us/windows/wsl/filesystems#disable-interoperability
            matches!(std::fs::read_to_string("/proc/sys/fs/binfmt_misc/WSLInterop").or_else(|_| std::fs::read_to_string("/proc/sys/fs/binfmt_misc/WSLInterop-late")), Ok(s) if s.contains("enabled"))
        } else {
            false
        };

        log::info!("gpui app started, initializing server");
        let session = start_server(listeners, log_rx, cx, is_wsl_interop);
        init_telemetry_forwarding(session.clone(), cx);
        trusted_worktrees::init(HashMap::default(), cx);

        GitHostingProviderRegistry::set_global(git_hosting_provider_registry, cx);
        git_hosting_providers::init(cx);
        dap_adapters::init(cx);

        extension::init(cx);
        let extension_host_proxy = ExtensionHostProxy::global(cx);

        json_schema_store::init(cx);

        let project = cx.new(|cx| {
            let fs = Arc::new(RealFs::new(None, cx.background_executor().clone()));
            let node_settings_rx = initialize_settings(session.clone(), fs.clone(), cx);

            let proxy_url = read_proxy_settings(cx);

            let http_client = {
                let _guard = Tokio::handle(cx).enter();
                Arc::new(
                    ReqwestClient::proxy_and_user_agent(
                        proxy_url,
                        &format!(
                            "Zed-Server/{} ({}; {})",
                            env!("CARGO_PKG_VERSION"),
                            std::env::consts::OS,
                            std::env::consts::ARCH
                        ),
                    )
                    .expect("Could not start HTTP client"),
                )
            };

            let node_runtime =
                NodeRuntime::new(http_client.clone(), shell_env_loaded_rx, node_settings_rx);

            let mut languages = LanguageRegistry::new(cx.background_executor().clone());
            languages.set_language_server_download_dir(paths::languages_dir().clone());
            let languages = Arc::new(languages);

            HeadlessProject::new(
                HeadlessAppState {
                    session: session.clone(),
                    fs,
                    http_client,
                    node_runtime,
                    languages,
                    extension_host_proxy,
                    startup_time,
                },
                true,
                cx,
            )
        });

        handle_crash_files_requests(&project, &session);

        cx.background_spawn(async move {
            cleanup_old_binaries_wsl();
            cleanup_old_binaries()
        })
        .detach();

        mem::forget(project);
    };
    // We do not reuse any of the state after unwinding, so we don't run risk of observing broken invariants.
    let app = std::panic::AssertUnwindSafe(app);
    let run = std::panic::AssertUnwindSafe(run);
    let res = std::panic::catch_unwind(move || { app }.0.run({ run }.0));
    if let Err(_) = res {
        log::error!("app panicked. quitting.");
        Err(anyhow::anyhow!("panicked"))
    } else {
        log::info!("gpui app is shut down. quitting.");
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ServerPathError {
    #[error("Failed to create server_dir `{path}`")]
    CreateServerDir {
        #[source]
        source: std::io::Error,
        path: PathBuf,
    },
    #[error("Failed to create logs_dir `{path}`")]
    CreateLogsDir {
        #[source]
        source: std::io::Error,
        path: PathBuf,
    },
}

#[derive(Clone, Debug)]
struct ServerPaths {
    log_file: PathBuf,
    launch_lock: PathBuf,
    pid_file: PathBuf,
    stdin_socket: PathBuf,
    stdout_socket: PathBuf,
    stderr_socket: PathBuf,
}

impl ServerPaths {
    fn new(identifier: &str) -> Result<Self, ServerPathError> {
        let server_dir = paths::remote_server_state_dir().join(identifier);
        std::fs::create_dir_all(&server_dir).map_err(|source| {
            ServerPathError::CreateServerDir {
                source,
                path: server_dir.clone(),
            }
        })?;
        let log_dir = logs_dir();
        std::fs::create_dir_all(log_dir).map_err(|source| ServerPathError::CreateLogsDir {
            source,
            path: log_dir.clone(),
        })?;

        let pid_file = server_dir.join("server.pid");
        let launch_lock = server_dir.join("server.lock");
        let stdin_socket = server_dir.join("stdin.sock");
        let stdout_socket = server_dir.join("stdout.sock");
        let stderr_socket = server_dir.join("stderr.sock");
        let log_file = logs_dir().join(format!("server-{}.log", identifier));

        Ok(Self {
            pid_file,
            launch_lock,
            stdin_socket,
            stdout_socket,
            stderr_socket,
            log_file,
        })
    }
}

#[derive(Debug, Error)]
pub enum ExecuteProxyError {
    #[error("Failed to init server paths: {0:#}")]
    ServerPath(#[from] ServerPathError),

    #[error(transparent)]
    ServerNotRunning(#[from] ProxyLaunchError),

    #[error("Failed to check PidFile '{path}': {source:#}")]
    CheckPidFile {
        #[source]
        source: CheckPidError,
        path: PathBuf,
    },

    #[error("Failed to lock remote server state '{path}': {source:#}")]
    LockServerState {
        #[source]
        source: std::io::Error,
        path: PathBuf,
    },

    #[error("Failed to kill existing server with pid '{pid}'")]
    KillRunningServer { pid: u32 },

    #[error("failed to spawn server")]
    SpawnServer(#[source] SpawnServerError),

    #[error("stdin_task failed: {0:#}")]
    StdinTask(#[source] anyhow::Error),
    #[error("stdout_task failed: {0:#}")]
    StdoutTask(#[source] anyhow::Error),
    #[error("stderr_task failed: {0:#}")]
    StderrTask(#[source] anyhow::Error),
}

impl ExecuteProxyError {
    pub fn to_exit_code(&self) -> i32 {
        match self {
            ExecuteProxyError::ServerNotRunning(proxy_launch_error) => {
                proxy_launch_error.to_exit_code()
            }
            _ => 1,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ServerLaunchAction {
    Attach(u32),
    Start,
    Restart(u32),
}

fn server_launch_action(
    proxy_mode: ProxyMode,
    running_pid: Option<u32>,
) -> Result<ServerLaunchAction, ProxyLaunchError> {
    match (proxy_mode, running_pid) {
        (ProxyMode::Start, Some(pid)) => Ok(ServerLaunchAction::Restart(pid)),
        (ProxyMode::Start | ProxyMode::ReconnectOrStart, None) => Ok(ServerLaunchAction::Start),
        (ProxyMode::Reconnect | ProxyMode::ReconnectOrStart, Some(pid)) => {
            Ok(ServerLaunchAction::Attach(pid))
        }
        (ProxyMode::Reconnect, None) => Err(ProxyLaunchError::ServerNotRunning),
    }
}

fn spawn_and_read_server_pid(paths: &ServerPaths) -> Result<u32, ExecuteProxyError> {
    gpui::block_on(spawn_server(paths)).map_err(ExecuteProxyError::SpawnServer)?;
    std::fs::read_to_string(&paths.pid_file)
        .and_then(|contents| {
            contents.parse::<u32>().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid PID file contents")
            })
        })
        .map_err(SpawnServerError::ProcessStatus)
        .map_err(ExecuteProxyError::SpawnServer)
}

pub(crate) fn execute_proxy(
    identifier: String,
    proxy_mode: ProxyMode,
) -> Result<(), ExecuteProxyError> {
    init_logging_proxy();

    let server_paths = ServerPaths::new(&identifier)?;

    let id = std::process::id().to_string();
    let should_install_crash_handler =
        client::telemetry::should_install_crash_handler(*RELEASE_CHANNEL);

    if should_install_crash_handler {
        smol::spawn(crashes::init(
            crashes::InitCrashHandler {
                session_id: id,
                zed_version: VERSION.to_owned(),
                binary: "zed-remote-proxy".to_string(),
                release_channel: release_channel::RELEASE_CHANNEL_NAME.clone(),
                commit_sha: option_env!("ZED_COMMIT_SHA").unwrap_or("no_sha").to_owned(),
            },
            |task| {
                smol::spawn(task).detach();
            },
            |pid| paths::temp_dir().join(format!("zed-remote-server-proxy-crash-handler-{pid}")),
            // we are running outside gpui
            #[allow(clippy::disallowed_methods)]
            |duration| FutureExt::map(Timer::after(duration), |_| ()),
        ))
        .detach();
    };
    log::info!("starting proxy process. PID: {}", std::process::id());
    let server_pid = {
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&server_paths.launch_lock)
            .map_err(|source| ExecuteProxyError::LockServerState {
                source,
                path: server_paths.launch_lock.clone(),
            })?;
        lock_file
            .lock()
            .map_err(|source| ExecuteProxyError::LockServerState {
                source,
                path: server_paths.launch_lock.clone(),
            })?;

        let server_pid = check_pid_file(&server_paths.pid_file).map_err(|source| {
            ExecuteProxyError::CheckPidFile {
                source,
                path: server_paths.pid_file.clone(),
            }
        })?;
        match server_launch_action(proxy_mode, server_pid)? {
            ServerLaunchAction::Attach(pid) => pid,
            ServerLaunchAction::Start => spawn_and_read_server_pid(&server_paths)?,
            ServerLaunchAction::Restart(pid) => {
                log::info!(
                    "proxy found server already running with PID {}. Killing process and cleaning up files...",
                    pid
                );
                kill_running_server(pid, &server_paths)?;
                spawn_and_read_server_pid(&server_paths)?
            }
        }
    };

    let stdin_task = smol::spawn(async move {
        let stdin = smol::Unblock::new(std::io::stdin());
        let stream = UnixStream::connect(&server_paths.stdin_socket)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to stdin socket {}",
                    server_paths.stdin_socket.display()
                )
            })?;
        handle_io(stdin, stream, "stdin").await
    });

    let stdout_task: smol::Task<Result<()>> = smol::spawn(async move {
        let stdout = smol::Unblock::new(std::io::stdout());
        let stream = UnixStream::connect(&server_paths.stdout_socket)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to stdout socket {}",
                    server_paths.stdout_socket.display()
                )
            })?;
        handle_io(stream, stdout, "stdout").await
    });

    let stderr_task: smol::Task<Result<()>> = smol::spawn(async move {
        let mut stderr = smol::Unblock::new(std::io::stderr());
        let mut stream = UnixStream::connect(&server_paths.stderr_socket)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to stderr socket {}",
                    server_paths.stderr_socket.display()
                )
            })?;
        let mut stderr_buffer = vec![0; 2048];
        loop {
            match stream
                .read(&mut stderr_buffer)
                .await
                .context("reading stderr")?
            {
                0 => {
                    let error =
                        std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "stderr closed");
                    Err(anyhow!(error))?;
                }
                n => {
                    stderr.write_all(&stderr_buffer[..n]).await?;
                    stderr.flush().await?;
                }
            }
        }
    });

    if let Err(forwarding_result) = gpui::block_on(async move {
        futures::select! {
            result = stdin_task.fuse() => result.map_err(ExecuteProxyError::StdinTask),
            result = stdout_task.fuse() => result.map_err(ExecuteProxyError::StdoutTask),
            result = stderr_task.fuse() => result.map_err(ExecuteProxyError::StderrTask),
        }
    }) {
        log::error!("encountered error while forwarding messages: {forwarding_result:#}",);
        if !matches!(gpui::block_on(check_server_running(server_pid)), Ok(true)) {
            log::error!("server exited unexpectedly");
            return Err(ExecuteProxyError::ServerNotRunning(
                ProxyLaunchError::ServerNotRunning,
            ));
        }
        return Err(forwarding_result);
    }

    Ok(())
}

fn kill_running_server(pid: u32, paths: &ServerPaths) -> Result<(), ExecuteProxyError> {
    log::info!("killing existing server with PID {}", pid);
    let system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );

    if let Some(process) = system.process(sysinfo::Pid::from_u32(pid)) {
        let killed = process.kill();
        if !killed {
            return Err(ExecuteProxyError::KillRunningServer { pid });
        }
    }

    for file in [
        &paths.pid_file,
        &paths.stdin_socket,
        &paths.stdout_socket,
        &paths.stderr_socket,
    ] {
        log::debug!("cleaning up file {:?} before starting new server", file);
        std::fs::remove_file(file).ok();
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum SpawnServerError {
    #[error("failed to remove stdin socket")]
    RemoveStdinSocket(#[source] std::io::Error),

    #[error("failed to remove stdout socket")]
    RemoveStdoutSocket(#[source] std::io::Error),

    #[error("failed to remove stderr socket")]
    RemoveStderrSocket(#[source] std::io::Error),

    #[error("failed to get current_exe")]
    CurrentExe(#[source] std::io::Error),

    #[error("failed to launch server process")]
    ProcessStatus(#[source] std::io::Error),

    #[error("failed to wait for server to be ready to accept connections")]
    Timeout,
}

async fn spawn_server(paths: &ServerPaths) -> Result<(), SpawnServerError> {
    log::info!("spawning server process",);
    if paths.stdin_socket.exists() {
        std::fs::remove_file(&paths.stdin_socket).map_err(SpawnServerError::RemoveStdinSocket)?;
    }
    if paths.stdout_socket.exists() {
        std::fs::remove_file(&paths.stdout_socket).map_err(SpawnServerError::RemoveStdoutSocket)?;
    }
    if paths.stderr_socket.exists() {
        std::fs::remove_file(&paths.stderr_socket).map_err(SpawnServerError::RemoveStderrSocket)?;
    }

    let binary_name = std::env::current_exe().map_err(SpawnServerError::CurrentExe)?;

    #[cfg(windows)]
    {
        spawn_server_windows(&binary_name, paths)?;
    }

    #[cfg(not(windows))]
    {
        spawn_server_normal(&binary_name, paths)?;
    }

    let mut total_time_waited = std::time::Duration::from_secs(0);
    let wait_duration = std::time::Duration::from_millis(20);
    while !paths.stdout_socket.exists()
        || !paths.stdin_socket.exists()
        || !paths.stderr_socket.exists()
    {
        log::debug!("waiting for server to be ready to accept connections...");
        std::thread::sleep(wait_duration);
        total_time_waited += wait_duration;
        if total_time_waited > std::time::Duration::from_secs(10) {
            return Err(SpawnServerError::Timeout);
        }
    }

    log::info!(
        "server ready to accept connections. total time waited: {:?}",
        total_time_waited
    );

    Ok(())
}

#[cfg(windows)]
fn spawn_server_windows(binary_name: &Path, paths: &ServerPaths) -> Result<(), SpawnServerError> {
    let binary_path = binary_name.to_string_lossy().to_string();
    let parameters = format!(
        "run --log-file \"{}\" --pid-file \"{}\" --stdin-socket \"{}\" --stdout-socket \"{}\" --stderr-socket \"{}\"",
        paths.log_file.to_string_lossy(),
        paths.pid_file.to_string_lossy(),
        paths.stdin_socket.to_string_lossy(),
        paths.stdout_socket.to_string_lossy(),
        paths.stderr_socket.to_string_lossy()
    );

    let directory = binary_name
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    crate::windows::shell_execute_from_explorer(&binary_path, &parameters, &directory)
        .map_err(|e| SpawnServerError::ProcessStatus(std::io::Error::other(e)))?;

    Ok(())
}

#[cfg(not(windows))]
fn spawn_server_normal(binary_name: &Path, paths: &ServerPaths) -> Result<(), SpawnServerError> {
    let mut server_process = new_command(binary_name);
    server_process
        .stdin(util::command::Stdio::null())
        .stdout(util::command::Stdio::null())
        .stderr(util::command::Stdio::null())
        .arg("run")
        .arg("--log-file")
        .arg(&paths.log_file)
        .arg("--pid-file")
        .arg(&paths.pid_file)
        .arg("--stdin-socket")
        .arg(&paths.stdin_socket)
        .arg("--stdout-socket")
        .arg(&paths.stdout_socket)
        .arg("--stderr-socket")
        .arg(&paths.stderr_socket);

    server_process
        .spawn()
        .map_err(SpawnServerError::ProcessStatus)?;

    Ok(())
}

#[derive(Debug, Error)]
#[error("Failed to remove PID file for missing process (pid `{pid}`")]
pub struct CheckPidError {
    #[source]
    source: std::io::Error,
    pid: u32,
}
async fn check_server_running(pid: u32) -> std::io::Result<bool> {
    new_command("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .await
        .map(|output| output.status.success())
}

fn check_pid_file(path: &Path) -> Result<Option<u32>, CheckPidError> {
    let Some(pid) = std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| contents.parse::<u32>().ok())
    else {
        return Ok(None);
    };

    log::debug!("Checking if process with PID {} exists...", pid);

    let system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );

    if system.process(sysinfo::Pid::from_u32(pid)).is_some() {
        log::debug!(
            "Process with PID {} exists. NOT spawning new server, but attaching to existing one.",
            pid
        );
        Ok(Some(pid))
    } else {
        log::debug!("Found PID file, but process with that PID does not exist. Removing PID file.");
        std::fs::remove_file(path).map_err(|source| CheckPidError { source, pid })?;
        Ok(None)
    }
}

fn write_pid_file(path: &Path, pid: u32) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    log::debug!("writing PID {} to file {:?}", pid, path);
    std::fs::write(path, pid.to_string()).context("Failed to write PID file")
}

async fn handle_io<R, W>(mut reader: R, mut writer: W, socket_name: &str) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use remote::protocol::{read_message_raw, write_size_prefixed_buffer};

    let mut buffer = Vec::new();
    loop {
        read_message_raw(&mut reader, &mut buffer)
            .await
            .with_context(|| format!("failed to read message from {}", socket_name))?;
        write_size_prefixed_buffer(&mut writer, &mut buffer)
            .await
            .with_context(|| format!("failed to write message to {}", socket_name))?;
        writer.flush().await?;
        buffer.clear();
    }
}

fn initialize_settings(
    session: AnyProtoClient,
    fs: Arc<dyn Fs>,
    cx: &mut App,
) -> watch::Receiver<Option<NodeBinaryOptions>> {
    let (user_settings_file_rx, watcher_task) =
        watch_config_file(cx.background_executor(), fs, paths::settings_file().clone());

    handle_settings_file_changes(user_settings_file_rx, watcher_task, cx, {
        move |err, _cx| {
            if let Some(e) = err {
                log::info!("Server settings failed to change: {}", e);

                session
                    .send(proto::Toast {
                        project_id: REMOTE_SERVER_PROJECT_ID,
                        notification_id: "server-settings-failed".to_string(),
                        message: format!(
                            "Error in settings on remote host {:?}: {}",
                            paths::settings_file(),
                            e
                        ),
                    })
                    .log_err();
            } else {
                session
                    .send(proto::HideToast {
                        project_id: REMOTE_SERVER_PROJECT_ID,
                        notification_id: "server-settings-failed".to_string(),
                    })
                    .log_err();
            }
        }
    });

    let (mut tx, rx) = watch::channel(None);
    let mut node_settings = None;
    cx.observe_global::<SettingsStore>(move |cx| {
        let new_node_settings = &ProjectSettings::get_global(cx).node;
        if Some(new_node_settings) != node_settings.as_ref() {
            log::info!("Got new node settings: {new_node_settings:?}");
            let options = NodeBinaryOptions {
                allow_path_lookup: !new_node_settings.ignore_system_version,
                // TODO: Implement this setting
                allow_binary_download: true,
                use_paths: new_node_settings.path.as_ref().map(|node_path| {
                    let node_path = PathBuf::from(shellexpand::tilde(node_path).as_ref());
                    let npm_path = new_node_settings
                        .npm_path
                        .as_ref()
                        .map(|path| PathBuf::from(shellexpand::tilde(&path).as_ref()));
                    (
                        node_path.clone(),
                        npm_path.unwrap_or_else(|| {
                            let base_path = PathBuf::new();
                            node_path.parent().unwrap_or(&base_path).join("npm")
                        }),
                    )
                }),
            };
            node_settings = Some(new_node_settings.clone());
            tx.send(Some(options)).ok();
        }
    })
    .detach();

    rx
}

pub fn handle_settings_file_changes(
    mut server_settings_file: mpsc::UnboundedReceiver<String>,
    watcher_task: gpui::Task<()>,
    cx: &mut App,
    settings_changed: impl Fn(Option<anyhow::Error>, &mut App) + 'static,
) {
    let server_settings_content = cx
        .foreground_executor()
        .block_on(server_settings_file.next())
        .unwrap();
    SettingsStore::update_global(cx, |store, cx| {
        store
            .set_server_settings(&server_settings_content, cx)
            .log_err();
    });
    cx.spawn(async move |cx| {
        let _watcher_task = watcher_task;
        while let Some(server_settings_content) = server_settings_file.next().await {
            cx.update_global(|store: &mut SettingsStore, cx| {
                let result = store.set_server_settings(&server_settings_content, cx);
                if let Err(err) = &result {
                    log::error!("Failed to load server settings: {err}");
                }
                settings_changed(result.err(), cx);
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

fn read_proxy_settings(cx: &mut Context<HeadlessProject>) -> Option<Url> {
    let proxy_str = ProxySettings::get_global(cx).proxy.to_owned();

    proxy_str
        .as_deref()
        .map(str::trim)
        .filter(|input| !input.is_empty())
        .and_then(|input| {
            input
                .parse::<Url>()
                .inspect_err(|e| log::error!("Error parsing proxy settings: {}", e))
                .ok()
        })
        .or_else(read_proxy_from_env)
}

fn cleanup_old_binaries() -> Result<()> {
    let server_dir = paths::remote_server_dir_relative();
    let release_channel = release_channel::RELEASE_CHANNEL.dev_name();
    let prefix = format!("zed-remote-server-{}-", release_channel);

    for entry in std::fs::read_dir(server_dir.as_std_path())? {
        let path = entry?.path();

        if let Some(file_name) = path.file_name()
            && let Some(version) = file_name.to_string_lossy().strip_prefix(&prefix)
            && !is_new_version(version)
            && !is_file_in_use(file_name)
        {
            log::info!("removing old remote server binary: {:?}", path);
            std::fs::remove_file(&path)?;
        }
    }

    Ok(())
}

// Remove this once 223 goes stable, we only have this to clean up old binaries on WSL
// we no longer download them into this folder, we use the same folder as other remote servers
fn cleanup_old_binaries_wsl() {
    let server_dir = paths::remote_wsl_server_dir_relative();
    if let Ok(()) = std::fs::remove_dir_all(server_dir.as_std_path()) {
        log::info!("removing old wsl remote server folder: {:?}", server_dir);
    }
}

fn is_new_version(version: &str) -> bool {
    semver::Version::from_str(version)
        .ok()
        .zip(semver::Version::from_str(env!("ZED_PKG_VERSION")).ok())
        .is_some_and(|(version, current_version)| version >= current_version)
}

fn is_file_in_use(file_name: &OsStr) -> bool {
    let info = sysinfo::System::new_with_specifics(sysinfo::RefreshKind::nothing().with_processes(
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    ));

    for process in info.processes().values() {
        if process
            .exe()
            .is_some_and(|exe| exe.file_name().is_some_and(|name| name == file_name))
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "debug-embed")]
    #[test]
    fn debug_remote_server_embeds_runtime_settings() {
        let default_settings = settings::default_settings();
        assert!(
            default_settings.contains("\"$schema\": \"zed://schemas/settings\""),
            "embedded settings must contain the canonical settings schema"
        );
    }

    #[test]
    fn reconnect_or_start_never_restarts_a_live_server() {
        assert_eq!(
            server_launch_action(ProxyMode::ReconnectOrStart, Some(42)).unwrap(),
            ServerLaunchAction::Attach(42)
        );
        assert_eq!(
            server_launch_action(ProxyMode::ReconnectOrStart, None).unwrap(),
            ServerLaunchAction::Start
        );
    }

    #[test]
    fn existing_start_and_reconnect_contracts_are_preserved() {
        assert_eq!(
            server_launch_action(ProxyMode::Start, Some(42)).unwrap(),
            ServerLaunchAction::Restart(42)
        );
        assert_eq!(
            server_launch_action(ProxyMode::Start, None).unwrap(),
            ServerLaunchAction::Start
        );
        assert_eq!(
            server_launch_action(ProxyMode::Reconnect, Some(42)).unwrap(),
            ServerLaunchAction::Attach(42)
        );
        assert!(matches!(
            server_launch_action(ProxyMode::Reconnect, None),
            Err(ProxyLaunchError::ServerNotRunning)
        ));
    }

    #[test]
    fn rotated_remote_log_path_uses_numbered_log_suffix() {
        assert_eq!(
            rotated_log_path(Path::new("server-workspace-12.log")),
            PathBuf::from("server-workspace-12.1.log")
        );
    }

    #[test]
    fn opening_remote_log_rotates_existing_oversized_log() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let log_path = temp_dir.path().join("server-workspace-12.log");
        let rotated_path = temp_dir.path().join("server-workspace-12.1.log");
        let existing_contents = vec![b'x'; REMOTE_SERVER_LOG_MAX_BYTES as usize];
        std::fs::write(&log_path, &existing_contents).expect("write oversized log");

        let _log_file = RotatingLogFile::open(&log_path).expect("open rotating log file");

        assert_eq!(
            std::fs::read(&rotated_path).expect("read rotated log"),
            existing_contents
        );
        assert_eq!(
            std::fs::metadata(&log_path)
                .expect("active log metadata")
                .len(),
            0
        );
    }

    #[test]
    fn writing_remote_log_rotates_before_exceeding_size_limit() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let log_path = temp_dir.path().join("server-workspace-12.log");
        let rotated_path = temp_dir.path().join("server-workspace-12.1.log");
        let existing_contents = vec![b'x'; REMOTE_SERVER_LOG_MAX_BYTES as usize - 1];
        let new_contents = b"yz";
        std::fs::write(&log_path, &existing_contents).expect("write existing log contents");
        let mut log_file = RotatingLogFile::open(&log_path).expect("open rotating log file");

        log_file
            .write_all(new_contents)
            .expect("write log contents");
        log_file.flush().expect("flush log file");

        assert_eq!(
            std::fs::read(&rotated_path).expect("read rotated log"),
            existing_contents
        );
        assert_eq!(
            std::fs::read(&log_path).expect("read active log"),
            new_contents
        );
    }
}
