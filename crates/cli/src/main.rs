#![allow(
    clippy::disallowed_methods,
    reason = "We are not in an async environment, so std::process::Command is fine"
)]
#![cfg_attr(
    any(target_os = "linux", target_os = "freebsd", target_os = "windows"),
    allow(dead_code)
)]

mod completions;

use crate::completions::Shell;

use anyhow::{Context as _, Result};
use clap::{CommandFactory, Parser};
use cli::{CliRequest, CliResponse, IpcHandshake, ipc::IpcOneShotServer};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};
use tempfile::{NamedTempFile, TempDir};
use util::paths::PathWithPosition;
use walkdir::WalkDir;

use std::io::IsTerminal;

const URL_PREFIX: [&'static str; 5] = ["zed://", "http://", "https://", "file://", "ssh://"];

struct Detect;

trait InstalledApp {
    fn zed_version_string(&self) -> String;
    fn launch(
        &self,
        ipc_url: String,
        user_data_dir: Option<&str>,
        launch_lane: &str,
    ) -> anyhow::Result<()>;
    fn record_cli_launch_failure(&self, _failure_class: &str, _lane: &str) {}
    fn run_foreground(
        &self,
        ipc_url: String,
        user_data_dir: Option<&str>,
        launch_lane: &str,
    ) -> io::Result<ExitStatus>;
    fn path(&self) -> PathBuf;
}

#[derive(Parser, Debug)]
#[command(
    name = "zed",
    disable_version_flag = true,
    before_help = "The Zed CLI binary.
This CLI is a separate binary that invokes Zed.

Examples:
    `zed`
          Simply opens Zed
    `zed --foreground`
          Runs in foreground (shows all logs)
    `zed path-to-your-project`
          Open your project in Zed
    `zed -n path-to-file `
          Open file/folder in a new window",
    after_help = "To read from stdin, append '-', e.g. 'ps axf | zed -'"
)]
struct Args {
    /// Wait for all of the given paths to be opened/closed before exiting.
    ///
    /// When opening a directory, waits until the created window is closed.
    #[arg(short, long)]
    wait: bool,
    /// Add files to the currently open workspace
    #[arg(short, long, overrides_with_all = ["new", "reuse", "existing", "classic"])]
    add: bool,
    /// Create a new workspace
    #[arg(short, long, overrides_with_all = ["add", "reuse", "existing", "classic"])]
    new: bool,
    /// Reuse an existing window, replacing its workspace
    #[arg(short, long, overrides_with_all = ["add", "new", "existing", "classic"], hide = true)]
    reuse: bool,
    /// Open in existing Zed window
    #[arg(short = 'e', long = "existing", overrides_with_all = ["add", "new", "reuse", "classic"])]
    existing: bool,
    /// Use the classic open behavior: new window for directories, reuse for files
    #[arg(long, hide = true, overrides_with_all = ["add", "new", "reuse", "existing"])]
    classic: bool,
    /// Sets a custom directory for all user data (e.g., database, extensions, logs).
    /// This overrides the default platform-specific data directory location:
    #[cfg_attr(target_os = "macos", doc = "`~/Library/Application Support/Zed`.")]
    #[cfg_attr(target_os = "windows", doc = "`%LOCALAPPDATA%\\Zed`.")]
    #[cfg_attr(
        not(any(target_os = "windows", target_os = "macos")),
        doc = "`$XDG_DATA_HOME/zed`."
    )]
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    user_data_dir: Option<String>,
    /// The paths to open in Zed (space-separated).
    ///
    /// Use `path:line:column` syntax to open a file at the given line and column.
    #[arg(trailing_var_arg = true, value_hint = clap::ValueHint::AnyPath)]
    paths_with_position: Vec<String>,
    /// Print Zed's version and the app path.
    #[arg(short, long)]
    version: bool,
    /// Run zed in the foreground (useful for debugging)
    #[arg(long)]
    foreground: bool,
    /// Custom path to Zed.app or the zed binary
    #[arg(long)]
    zed: Option<PathBuf>,
    /// Run zed in dev-server mode
    #[arg(long)]
    dev_server_token: Option<String>,
    /// The username and WSL distribution to use when opening paths. If not specified,
    /// Zed will attempt to open the paths directly.
    ///
    /// The username is optional, and if not specified, the default user for the distribution
    /// will be used.
    ///
    /// Example: `me@Ubuntu` or `Ubuntu`.
    ///
    /// WARN: You should not fill in this field by hand.
    #[cfg(target_os = "windows")]
    #[arg(long, value_name = "USER@DISTRO")]
    wsl: Option<String>,
    /// Not supported in Zed CLI, only supported on Zed binary
    /// Will attempt to give the correct command to run
    #[arg(long)]
    system_specs: bool,
    /// Open the project in a dev container.
    ///
    /// Automatically triggers "Reopen in Dev Container" if a `.devcontainer/`
    /// configuration is found in the project directory.
    #[arg(long)]
    dev_container: bool,
    /// Pairs of file paths to diff. Can be specified multiple times.
    /// When directories are provided, recurses into them and shows all changed files in a single multi-diff view.
    #[arg(long, action = clap::ArgAction::Append, num_args = 2, value_names = ["OLD_PATH", "NEW_PATH"], value_hint = clap::ValueHint::AnyPath)]
    diff: Vec<String>,
    /// Generate shell completions for Zed
    #[arg(long, value_names = ["SHELL"])]
    completions: Option<Shell>,
    /// Uninstall Zed from user system
    #[cfg(all(
        any(target_os = "linux", target_os = "macos"),
        not(feature = "no-bundled-uninstall")
    ))]
    #[arg(long)]
    uninstall: bool,

    /// Used for SSH/Git password authentication, to remove the need for netcat as a dependency,
    /// by having Zed act like netcat communicating over a Unix socket.
    #[arg(long, hide = true)]
    askpass: Option<String>,
}

/// Parses a path containing a position (e.g. `path:line:column`)
/// and returns its canonicalized string representation.
///
/// If a part of path doesn't exist, it will canonicalize the
/// existing part and append the non-existing part.
///
/// This method must return an absolute path, as many zed
/// crates assume absolute paths.
fn parse_path_with_position(argument_str: &str) -> anyhow::Result<String> {
    match Path::new(argument_str).canonicalize() {
        Ok(existing_path) => Ok(PathWithPosition::from_path(existing_path)),
        Err(_) => PathWithPosition::parse_str(argument_str).map_path(|mut path| {
            let curdir = env::current_dir().context("retrieving current directory")?;
            let mut children = Vec::new();
            let root;
            loop {
                // canonicalize handles './', and '/'.
                if let Ok(canonicalized) = fs::canonicalize(&path) {
                    root = canonicalized;
                    break;
                }
                // The comparison to `curdir` is just a shortcut
                // since we know it is canonical. The other one
                // is if `argument_str` is a string that starts
                // with a name (e.g. "foo/bar").
                if path == curdir || path == Path::new("") {
                    root = curdir;
                    break;
                }
                children.push(
                    path.file_name()
                        .with_context(|| format!("parsing as path with position {argument_str}"))?
                        .to_owned(),
                );
                if !path.pop() {
                    unreachable!("parsing as path with position {argument_str}");
                }
            }
            Ok(children.iter().rev().fold(root, |mut path, child| {
                path.push(child);
                path
            }))
        }),
    }
    .map(|path_with_pos| path_with_pos.to_string(&|path| path.to_string_lossy().into_owned()))
}

/// Returns whether a `--diff` argument refers to an existing path, allowing a
/// trailing `:line:column` suffix (parsed later by the Zed side, matching how
/// regular `zed path:line:column` arguments are handled).
fn diff_path_exists(diff_path: &str) -> bool {
    Path::new(diff_path).exists() || PathWithPosition::parse_str(diff_path).path.exists()
}

fn expand_directory_diff_pairs(
    diff_pairs: Vec<[String; 2]>,
) -> anyhow::Result<(Vec<[String; 2]>, Vec<TempDir>)> {
    let mut expanded = Vec::new();
    let mut temp_dirs = Vec::new();

    for pair in diff_pairs {
        let left = PathBuf::from(&pair[0]);
        let right = PathBuf::from(&pair[1]);

        if left.is_dir() && right.is_dir() {
            let (mut pairs, temp_dir) = expand_directory_pair(&left, &right)?;
            expanded.append(&mut pairs);
            if let Some(temp_dir) = temp_dir {
                temp_dirs.push(temp_dir);
            }
        } else {
            expanded.push(pair);
        }
    }

    Ok((expanded, temp_dirs))
}

fn expand_directory_pair(
    left: &Path,
    right: &Path,
) -> anyhow::Result<(Vec<[String; 2]>, Option<TempDir>)> {
    let left_files = collect_files(left)?;
    let right_files = collect_files(right)?;

    let mut rel_paths = BTreeSet::new();
    rel_paths.extend(left_files.keys().cloned());
    rel_paths.extend(right_files.keys().cloned());

    let mut temp_dir = TempDir::new()?;
    let mut temp_dir_used = false;
    let mut pairs = Vec::new();

    for rel in rel_paths {
        match (left_files.get(&rel), right_files.get(&rel)) {
            (Some(left_path), Some(right_path)) => {
                pairs.push([
                    left_path.to_string_lossy().into_owned(),
                    right_path.to_string_lossy().into_owned(),
                ]);
            }
            (Some(left_path), None) => {
                let stub = create_empty_stub(&mut temp_dir, &rel)?;
                temp_dir_used = true;
                pairs.push([
                    left_path.to_string_lossy().into_owned(),
                    stub.to_string_lossy().into_owned(),
                ]);
            }
            (None, Some(right_path)) => {
                let stub = create_empty_stub(&mut temp_dir, &rel)?;
                temp_dir_used = true;
                pairs.push([
                    stub.to_string_lossy().into_owned(),
                    right_path.to_string_lossy().into_owned(),
                ]);
            }
            (None, None) => {}
        }
    }

    let temp_dir = if temp_dir_used { Some(temp_dir) } else { None };
    Ok((pairs, temp_dir))
}

fn collect_files(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, PathBuf>> {
    let mut files = BTreeMap::new();

    for entry in WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let rel = entry
                .path()
                .strip_prefix(root)
                .context("stripping directory prefix")?
                .to_path_buf();
            files.insert(rel, entry.into_path());
        }
    }

    Ok(files)
}

fn create_empty_stub(temp_dir: &mut TempDir, rel: &Path) -> anyhow::Result<PathBuf> {
    let stub_path = temp_dir.path().join(rel);
    if let Some(parent) = stub_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(&stub_path)?;
    Ok(stub_path)
}

const APP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
enum AppHandshakeFailure {
    App(String),
    Timeout(Duration),
    WorkerDisconnected,
}

impl AppHandshakeFailure {
    fn failure_class(&self) -> &'static str {
        match self {
            Self::App(_) => "handshake_failed",
            Self::Timeout(_) => "handshake_timeout",
            Self::WorkerDisconnected => "handshake_worker_exit",
        }
    }
}

impl std::fmt::Display for AppHandshakeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(error) => {
                write!(
                    formatter,
                    "Zed failed before completing the CLI handshake: {error}"
                )
            }
            Self::Timeout(timeout) => write!(
                formatter,
                "timed out after {timeout:?} waiting for Zed to complete the CLI handshake; \
                 the app launch request was accepted but no matching Zed instance connected"
            ),
            Self::WorkerDisconnected => {
                write!(
                    formatter,
                    "Zed CLI handshake worker exited before connecting"
                )
            }
        }
    }
}

impl std::error::Error for AppHandshakeFailure {}

fn wait_for_app_handshake(
    receiver: mpsc::Receiver<Result<(), String>>,
    timeout: Duration,
) -> std::result::Result<(), AppHandshakeFailure> {
    match receiver.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(AppHandshakeFailure::App(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AppHandshakeFailure::Timeout(timeout)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AppHandshakeFailure::WorkerDisconnected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use util::path;
    use util::paths::SanitizedPath;
    use util::test::TempTree;

    macro_rules! assert_path_eq {
        ($left:expr, $right:expr) => {
            assert_eq!(
                SanitizedPath::new(Path::new(&$left)),
                SanitizedPath::new(Path::new(&$right))
            )
        };
    }

    fn cwd() -> PathBuf {
        env::current_dir().unwrap()
    }

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn with_cwd<T>(path: &Path, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let _lock = CWD_LOCK.lock();
        let old_cwd = cwd();
        env::set_current_dir(path)?;
        let result = f();
        env::set_current_dir(old_cwd)?;
        result
    }

    #[test]
    fn test_parse_non_existing_path() {
        // Absolute path
        let result = parse_path_with_position(path!("/non/existing/path.txt")).unwrap();
        assert_path_eq!(result, path!("/non/existing/path.txt"));

        // Absolute path in cwd
        let path = cwd().join(path!("non/existing/path.txt"));
        let expected = path.to_string_lossy().to_string();
        let result = parse_path_with_position(&expected).unwrap();
        assert_path_eq!(result, expected);

        // Relative path
        let result = parse_path_with_position(path!("non/existing/path.txt")).unwrap();
        assert_path_eq!(result, expected)
    }

    #[test]
    fn test_parse_existing_path() {
        let temp_tree = TempTree::new(json!({
            "file.txt": "",
        }));
        let file_path = temp_tree.path().join("file.txt");
        let expected = file_path.to_string_lossy().to_string();

        // Absolute path
        let result = parse_path_with_position(file_path.to_str().unwrap()).unwrap();
        assert_path_eq!(result, expected);

        // Relative path
        let result = with_cwd(temp_tree.path(), || parse_path_with_position("file.txt")).unwrap();
        assert_path_eq!(result, expected);
    }

    // NOTE:
    // While POSIX symbolic links are somewhat supported on Windows, they are an opt in by the user, and thus
    // we assume that they are not supported out of the box.
    #[cfg(not(windows))]
    #[test]
    fn test_parse_symlink_file() {
        let temp_tree = TempTree::new(json!({
            "target.txt": "",
        }));
        let target_path = temp_tree.path().join("target.txt");
        let symlink_path = temp_tree.path().join("symlink.txt");
        std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();

        // Absolute path
        let result = parse_path_with_position(symlink_path.to_str().unwrap()).unwrap();
        assert_eq!(result, target_path.to_string_lossy());

        // Relative path
        let result =
            with_cwd(temp_tree.path(), || parse_path_with_position("symlink.txt")).unwrap();
        assert_eq!(result, target_path.to_string_lossy());
    }

    #[cfg(not(windows))]
    #[test]
    fn test_parse_symlink_dir() {
        let temp_tree = TempTree::new(json!({
            "some": {
                "dir": { // symlink target
                    "ec": {
                        "tory": {
                            "file.txt": "",
        }}}}}));

        let target_file_path = temp_tree.path().join("some/dir/ec/tory/file.txt");
        let expected = target_file_path.to_string_lossy();

        let dir_path = temp_tree.path().join("some/dir");
        let symlink_path = temp_tree.path().join("symlink");
        std::os::unix::fs::symlink(&dir_path, &symlink_path).unwrap();

        // Absolute path
        let result =
            parse_path_with_position(symlink_path.join("ec/tory/file.txt").to_str().unwrap())
                .unwrap();
        assert_eq!(result, expected);

        // Relative path
        let result = with_cwd(temp_tree.path(), || {
            parse_path_with_position("symlink/ec/tory/file.txt")
        })
        .unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_cli_handshake_timeout_is_bounded() {
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let error =
            wait_for_app_handshake(receiver, std::time::Duration::from_millis(1)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("timed out after 1ms waiting for Zed to complete the CLI handshake"),
            "{error:#}"
        );
    }
}

fn parse_path_in_wsl(source: &str, wsl: &str) -> Result<String> {
    let mut source = PathWithPosition::parse_str(source);

    let (user, distro_name) = if let Some((user, distro)) = wsl.split_once('@') {
        if user.is_empty() {
            anyhow::bail!("user is empty in wsl argument");
        }
        (Some(user), distro)
    } else {
        (None, wsl)
    };

    let mut args = vec!["--distribution", distro_name];
    if let Some(user) = user {
        args.push("--user");
        args.push(user);
    }

    let command = [
        OsStr::new("realpath"),
        OsStr::new("-s"),
        source.path.as_ref(),
    ];

    let output = util::command::new_std_command("wsl.exe")
        .args(&args)
        .arg("--exec")
        .args(&command)
        .output()?;
    let result = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        let fallback = util::command::new_std_command("wsl.exe")
            .args(&args)
            .arg("--")
            .args(&command)
            .output()?;
        String::from_utf8_lossy(&fallback.stdout).to_string()
    };

    source.path = Path::new(result.trim()).to_owned();

    Ok(source.to_string(&|path| path.to_string_lossy().into_owned()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    #[cfg(unix)]
    util::prevent_root_execution();

    // Exit flatpak sandbox if needed
    #[cfg(target_os = "linux")]
    {
        flatpak::try_restart_to_host();
        flatpak::ld_extra_libs();
    }

    // Intercept version designators
    #[cfg(target_os = "macos")]
    if let Some(channel) = std::env::args().nth(1).filter(|arg| arg.starts_with("--")) {
        // When the first argument is a name of a release channel, we're going to spawn off the CLI of that version, with trailing args passed along.
        use std::str::FromStr as _;

        if let Ok(channel) = release_channel::ReleaseChannel::from_str(&channel[2..]) {
            return mac_os::spawn_channel_cli(channel, std::env::args().skip(2).collect());
        }
    }

    // Must happen before clap — SSH invokes cli.exe directly as SSH_ASKPASS
    // and passes the socket path via env var to avoid argument parsing.
    if let Ok(socket) = std::env::var("ZED_ASKPASS_SOCKET") {
        askpass::main_from_args(&socket, std::env::args().skip(1));
        return Ok(());
    }

    let args = Args::parse();

    // `zed --askpass` Makes zed operate in nc/netcat mode for use with askpass
    if let Some(socket) = &args.askpass {
        askpass::main(socket);
        return Ok(());
    }

    // Set custom data directory before any path operations
    let user_data_dir = args.user_data_dir.clone();
    if let Some(dir) = &user_data_dir {
        paths::set_custom_data_dir(dir);
    }

    #[cfg(target_os = "linux")]
    let args = flatpak::set_bin_if_no_escape(args);

    let app = Detect::detect(args.zed.as_deref()).context("Bundle detection")?;

    if let Some(shell) = &args.completions {
        let file_path = std::env::current_exe()?;
        let file_name = file_path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or("--completions expects a UTF-8 name for cli bin")
            .map_err(anyhow::Error::msg)?;
        let mut cmd = Args::command();
        cmd.set_bin_name(file_name);
        cmd.build();
        crate::completions::main(&cmd, shell);
        return Ok(());
    }

    if args.version {
        println!("{}", app.zed_version_string());
        return Ok(());
    }

    if args.system_specs {
        let path = app.path();
        let msg = [
            "The `--system-specs` argument is not supported in the Zed CLI, only on Zed binary.",
            "To retrieve the system specs on the command line, run the following command:",
            &format!("{} --system-specs", path.display()),
        ];
        anyhow::bail!(msg.join("\n"));
    }

    #[cfg(all(
        any(target_os = "linux", target_os = "macos"),
        not(feature = "no-bundled-uninstall")
    ))]
    if args.uninstall {
        static UNINSTALL_SCRIPT: &[u8] = include_bytes!("../../../script/uninstall.sh");

        let tmp_dir = tempfile::tempdir()?;
        let script_path = tmp_dir.path().join("uninstall.sh");
        fs::write(&script_path, UNINSTALL_SCRIPT)?;

        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

        let status = std::process::Command::new("sh")
            .arg(&script_path)
            .env("ZED_CHANNEL", &*release_channel::RELEASE_CHANNEL_NAME)
            .status()
            .context("Failed to execute uninstall script")?;

        std::process::exit(status.code().unwrap_or(1));
    }

    let (server, server_name) =
        IpcOneShotServer::<IpcHandshake>::new().context("Handshake before Zed spawn")?;
    let url = format!("zed-cli://{server_name}");

    let open_behavior = if args.new {
        cli::OpenBehavior::AlwaysNew
    } else if args.add {
        cli::OpenBehavior::Add
    } else if args.existing {
        cli::OpenBehavior::ExistingWindow
    } else if args.classic {
        cli::OpenBehavior::Classic
    } else if args.reuse {
        cli::OpenBehavior::Reuse
    } else {
        cli::OpenBehavior::Default
    };

    let env = {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            use collections::HashMap;

            // On Linux, the desktop entry uses `cli` to spawn `zed`.
            // We need to handle env vars correctly since std::env::vars() may not contain
            // project-specific vars (e.g. those set by direnv).
            // By setting env to None here, the LSP will use worktree env vars instead,
            // which is what we want.
            if !std::io::stdout().is_terminal() {
                None
            } else {
                Some(std::env::vars().collect::<HashMap<_, _>>())
            }
        }

        #[cfg(target_os = "windows")]
        {
            // On Windows, by default, a child process inherits a copy of the environment block of the parent process.
            // So we don't need to pass env vars explicitly.
            None
        }

        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
        {
            use collections::HashMap;

            Some(std::env::vars().collect::<HashMap<_, _>>())
        }
    };

    let exit_status = Arc::new(Mutex::new(None));
    let mut paths = vec![];
    let mut urls = vec![];
    let mut diff_paths = vec![];
    let mut stdin_tmp_file: Option<fs::File> = None;
    let mut anonymous_fd_tmp_files = vec![];

    // Check if any diff paths are directories to determine diff_all mode
    let diff_all_mode = args
        .diff
        .chunks(2)
        .any(|pair| Path::new(&pair[0]).is_dir() || Path::new(&pair[1]).is_dir());

    for path in args.diff.chunks(2) {
        let left = parse_path_with_position(&path[0])?;
        let right = parse_path_with_position(&path[1])?;
        for diff_path in [&left, &right] {
            anyhow::ensure!(
                diff_path_exists(diff_path),
                "--diff path does not exist: {diff_path}"
            );
        }
        diff_paths.push([left, right]);
    }

    let (expanded_diff_paths, temp_dirs) = expand_directory_diff_pairs(diff_paths)?;
    diff_paths = expanded_diff_paths;
    // Prevent automatic cleanup of temp directories containing empty stub files
    // for directory diffs. The CLI process may exit before Zed has read these
    // files (e.g., when RPC-ing into an already-running instance). The files
    // live in the OS temp directory and will be cleaned up on reboot.
    for temp_dir in temp_dirs {
        let _ = temp_dir.keep();
    }

    #[cfg(target_os = "windows")]
    let wsl = args.wsl.as_ref();
    #[cfg(not(target_os = "windows"))]
    let wsl = None;

    for path in args.paths_with_position.iter() {
        if URL_PREFIX.iter().any(|&prefix| path.starts_with(prefix)) {
            urls.push(path.to_string());
        } else if path == "-" && args.paths_with_position.len() == 1 {
            let file = NamedTempFile::new()?;
            paths.push(file.path().to_string_lossy().into_owned());
            let (file, _) = file.keep()?;
            stdin_tmp_file = Some(file);
        } else if let Some(file) = anonymous_fd(path) {
            let tmp_file = NamedTempFile::new()?;
            paths.push(tmp_file.path().to_string_lossy().into_owned());
            let (tmp_file, _) = tmp_file.keep()?;
            anonymous_fd_tmp_files.push((file, tmp_file));
        } else if let Some(wsl) = wsl {
            urls.push(format!("file://{}", parse_path_in_wsl(path, wsl)?));
        } else {
            paths.push(parse_path_with_position(path)?);
        }
    }
    let launch_lane = if urls
        .iter()
        .any(|url| url.starts_with("ssh://") || url.starts_with("zed://ssh/"))
    {
        "intrepid"
    } else {
        "local"
    };

    anyhow::ensure!(
        args.dev_server_token.is_none(),
        "Dev servers were removed in v0.157.x please upgrade to SSH remoting: https://zed.dev/docs/remote-development"
    );

    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .stack_size(10 * 1024 * 1024)
        .thread_name(|ix| format!("RayonWorker{}", ix))
        .build_global()
        .unwrap();

    let (handshake_status_tx, handshake_status_rx) = mpsc::sync_channel(1);
    let sender: JoinHandle<anyhow::Result<()>> = thread::Builder::new()
        .name("CliReceiver".to_string())
        .spawn({
            let exit_status = exit_status.clone();
            let user_data_dir_for_thread = user_data_dir.clone();
            move || {
                let (_, handshake) = match server.accept().context("Handshake after Zed spawn") {
                    Ok(handshake) => {
                        let _ = handshake_status_tx.send(Ok(()));
                        handshake
                    }
                    Err(error) => {
                        let _ = handshake_status_tx.send(Err(format!("{error:#}")));
                        return Err(error);
                    }
                };
                let (tx, rx) = (handshake.requests, handshake.responses);

                #[cfg(target_os = "windows")]
                let wsl = args.wsl;
                #[cfg(not(target_os = "windows"))]
                let wsl = None;

                let open_request = CliRequest::Open {
                    paths,
                    urls,
                    diff_paths,
                    diff_all: diff_all_mode,
                    wsl,
                    wait: args.wait,
                    open_behavior,
                    env,
                    user_data_dir: user_data_dir_for_thread,
                    dev_container: args.dev_container,
                    cwd: env::current_dir().ok(),
                };

                tx.send(open_request)?;

                while let Ok(response) = rx.recv() {
                    match response {
                        CliResponse::Ping => {}
                        CliResponse::Stdout { message } => println!("{message}"),
                        CliResponse::Stderr { message } => eprintln!("{message}"),
                        CliResponse::Exit { status } => {
                            exit_status.lock().replace(status);
                            return Ok(());
                        }
                        CliResponse::PromptOpenBehavior => {
                            let behavior = prompt_open_behavior()
                                .unwrap_or(cli::CliBehaviorSetting::ExistingWindow);
                            tx.send(CliRequest::SetOpenBehavior { behavior })?;
                        }
                    }
                }

                Ok(())
            }
        })
        .unwrap();

    let stdin_pipe_handle: Option<JoinHandle<anyhow::Result<()>>> =
        stdin_tmp_file.map(|mut tmp_file| {
            thread::Builder::new()
                .name("CliStdin".to_string())
                .spawn(move || {
                    let mut stdin = std::io::stdin().lock();
                    if !io::IsTerminal::is_terminal(&stdin) {
                        io::copy(&mut stdin, &mut tmp_file)?;
                    }
                    Ok(())
                })
                .unwrap()
        });

    let anonymous_fd_pipe_handles: Vec<_> = anonymous_fd_tmp_files
        .into_iter()
        .map(|(mut file, mut tmp_file)| {
            thread::Builder::new()
                .name("CliAnonymousFd".to_string())
                .spawn(move || io::copy(&mut file, &mut tmp_file))
                .unwrap()
        })
        .collect();

    if args.foreground {
        app.run_foreground(url, user_data_dir.as_deref(), launch_lane)?;
    } else {
        if let Err(error) = app.launch(url, user_data_dir.as_deref(), launch_lane) {
            app.record_cli_launch_failure("launcher_spawn_failed", launch_lane);
            return Err(error);
        }
        if let Err(error) = wait_for_app_handshake(handshake_status_rx, APP_HANDSHAKE_TIMEOUT) {
            app.record_cli_launch_failure(error.failure_class(), launch_lane);
            return Err(error.into());
        }
        sender.join().unwrap()?;
        if let Some(handle) = stdin_pipe_handle {
            handle.join().unwrap()?;
        }
        for handle in anonymous_fd_pipe_handles {
            handle.join().unwrap()?;
        }
    }

    if let Some(exit_status) = exit_status.lock().take() {
        std::process::exit(exit_status);
    }
    Ok(())
}

fn anonymous_fd(path: &str) -> Option<fs::File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{self, FromRawFd};

        let fd_str = path.strip_prefix("/proc/self/fd/")?;

        let link = fs::read_link(path).ok()?;
        if !link.starts_with("memfd:") {
            return None;
        }

        let fd: fd::RawFd = fd_str.parse().ok()?;
        let file = unsafe { fs::File::from_raw_fd(fd) };
        Some(file)
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        use std::os::{
            fd::{self, FromRawFd},
            unix::fs::FileTypeExt,
        };

        let fd_str = path.strip_prefix("/dev/fd/")?;

        let metadata = fs::metadata(path).ok()?;
        let file_type = metadata.file_type();
        if !file_type.is_fifo() && !file_type.is_socket() {
            return None;
        }
        let fd: fd::RawFd = fd_str.parse().ok()?;
        let file = unsafe { fs::File::from_raw_fd(fd) };
        Some(file)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        _ = path;
        // not implemented for bsd, windows. Could be, but isn't yet
        None
    }
}

/// Shows an interactive prompt asking the user to choose the default open
/// behavior for `zed <path>`. Returns `None` if the prompt cannot be shown
/// (e.g. stdin is not a terminal) or the user cancels.
fn prompt_open_behavior() -> Option<cli::CliBehaviorSetting> {
    if !std::io::stdin().is_terminal() {
        return None;
    }

    let blue = console::Style::new().blue();
    let items = [
        format!(
            "Add to existing Zed window ({})",
            blue.apply_to("zed --existing")
        ),
        format!("Open a new window ({})", blue.apply_to("zed --classic")),
    ];

    let prompt = format!(
        "Configure default behavior for {}\n{}",
        blue.apply_to("zed <path>"),
        console::style("You can change this later in Zed settings"),
    );

    let selection = dialoguer::Select::new()
        .with_prompt(&prompt)
        .items(&items)
        .default(0)
        .interact()
        .ok()?;

    Some(if selection == 0 {
        cli::CliBehaviorSetting::ExistingWindow
    } else {
        cli::CliBehaviorSetting::NewWindow
    })
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux {
    use std::{
        env,
        ffi::OsString,
        io,
        os::unix::net::{SocketAddr, UnixDatagram},
        path::{Path, PathBuf},
        process::{self, ExitStatus},
        thread,
        time::Duration,
    };

    use anyhow::{Context as _, anyhow};
    use cli::FORCE_CLI_MODE_ENV_VAR_NAME;
    use fork::Fork;

    use crate::{Detect, InstalledApp};

    struct App(PathBuf);

    impl Detect {
        pub fn detect(path: Option<&Path>) -> anyhow::Result<impl InstalledApp> {
            let path = if let Some(path) = path {
                path.to_path_buf().canonicalize()?
            } else {
                let cli = env::current_exe()?;
                let dir = cli.parent().context("no parent path for cli")?;

                // libexec is the standard, lib/zed is for Arch (and other non-libexec distros),
                // ./zed is for the target directory in development builds.
                let possible_locations =
                    ["../libexec/zed-editor", "../lib/zed/zed-editor", "./zed"];
                possible_locations
                    .iter()
                    .find_map(|p| dir.join(p).canonicalize().ok().filter(|path| path != &cli))
                    .with_context(|| {
                        format!("could not find any of: {}", possible_locations.join(", "))
                    })?
            };

            Ok(App(path))
        }
    }

    impl InstalledApp for App {
        fn zed_version_string(&self) -> String {
            format!(
                "Zed {}{}{} – {}",
                if *release_channel::RELEASE_CHANNEL_NAME == "stable" {
                    "".to_string()
                } else {
                    format!("{} ", *release_channel::RELEASE_CHANNEL_NAME)
                },
                option_env!("RELEASE_VERSION").unwrap_or_default(),
                match option_env!("ZED_COMMIT_SHA") {
                    Some(commit_sha) => format!(" {commit_sha} "),
                    None => "".to_string(),
                },
                self.0.display(),
            )
        }

        fn launch(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
            _launch_lane: &str,
        ) -> anyhow::Result<()> {
            let data_dir = user_data_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| paths::data_dir().clone());

            let sock_path = data_dir.join(format!(
                "zed-{}.sock",
                *release_channel::RELEASE_CHANNEL_NAME
            ));
            let sock = UnixDatagram::unbound()?;
            if sock.connect(&sock_path).is_err() {
                self.boot_background(ipc_url, user_data_dir)?;
            } else {
                sock.send(ipc_url.as_bytes())?;
            }
            Ok(())
        }

        fn run_foreground(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
            _launch_lane: &str,
        ) -> io::Result<ExitStatus> {
            let mut cmd = std::process::Command::new(self.0.clone());
            cmd.arg(ipc_url);
            if let Some(dir) = user_data_dir {
                cmd.arg("--user-data-dir").arg(dir);
            }
            cmd.status()
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl App {
        fn boot_background(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
        ) -> anyhow::Result<()> {
            let path = &self.0;

            match fork::fork() {
                Ok(Fork::Parent(_)) => Ok(()),
                Ok(Fork::Child) => {
                    unsafe { std::env::set_var(FORCE_CLI_MODE_ENV_VAR_NAME, "") };
                    if fork::setsid().is_err() {
                        eprintln!("failed to setsid: {}", std::io::Error::last_os_error());
                        process::exit(1);
                    }
                    if fork::close_fd().is_err() {
                        eprintln!("failed to close_fd: {}", std::io::Error::last_os_error());
                    }
                    let mut args: Vec<OsString> =
                        vec![path.as_os_str().to_owned(), OsString::from(ipc_url)];
                    if let Some(dir) = user_data_dir {
                        args.push(OsString::from("--user-data-dir"));
                        args.push(OsString::from(dir));
                    }
                    let error = exec::execvp(path.clone(), &args);
                    // if exec succeeded, we never get here.
                    eprintln!("failed to exec {:?}: {}", path, error);
                    process::exit(1)
                }
                Err(_) => Err(anyhow!(io::Error::last_os_error())),
            }
        }

        fn wait_for_socket(
            &self,
            sock_addr: &SocketAddr,
            sock: &mut UnixDatagram,
        ) -> Result<(), std::io::Error> {
            for _ in 0..100 {
                thread::sleep(Duration::from_millis(10));
                if sock.connect_addr(sock_addr).is_ok() {
                    return Ok(());
                }
            }
            sock.connect_addr(sock_addr)
        }
    }
}

#[cfg(target_os = "linux")]
mod flatpak {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::Command;
    use std::{env, process};

    const EXTRA_LIB_ENV_NAME: &str = "ZED_FLATPAK_LIB_PATH";
    const NO_ESCAPE_ENV_NAME: &str = "ZED_FLATPAK_NO_ESCAPE";

    /// Adds bundled libraries to LD_LIBRARY_PATH if running under flatpak
    pub fn ld_extra_libs() {
        let mut paths = if let Ok(paths) = env::var("LD_LIBRARY_PATH") {
            env::split_paths(&paths).collect()
        } else {
            Vec::new()
        };

        if let Ok(extra_path) = env::var(EXTRA_LIB_ENV_NAME) {
            paths.push(extra_path.into());
        }

        unsafe { env::set_var("LD_LIBRARY_PATH", env::join_paths(paths).unwrap()) };
    }

    /// Restarts outside of the sandbox if currently running within it
    pub fn try_restart_to_host() {
        if let Some(flatpak_dir) = get_flatpak_dir() {
            let mut args = vec!["/usr/bin/flatpak-spawn".into(), "--host".into()];
            args.append(&mut get_xdg_env_args());
            args.push("--env=ZED_UPDATE_EXPLANATION=Please use flatpak to update zed".into());
            args.push(
                format!(
                    "--env={EXTRA_LIB_ENV_NAME}={}",
                    flatpak_dir.join("lib").to_str().unwrap()
                )
                .into(),
            );
            args.push(flatpak_dir.join("bin").join("zed").into());

            let mut is_app_location_set = false;
            for arg in &env::args_os().collect::<Vec<_>>()[1..] {
                args.push(arg.clone());
                is_app_location_set |= arg == "--zed";
            }

            if !is_app_location_set {
                args.push("--zed".into());
                args.push(flatpak_dir.join("libexec").join("zed-editor").into());
            }

            let error = exec::execvp("/usr/bin/flatpak-spawn", args);
            eprintln!("failed restart cli on host: {:?}", error);
            process::exit(1);
        }
    }

    pub fn set_bin_if_no_escape(mut args: super::Args) -> super::Args {
        if env::var(NO_ESCAPE_ENV_NAME).is_ok()
            && env::var("FLATPAK_ID").is_ok_and(|id| id.starts_with("dev.zed.Zed"))
            && args.zed.is_none()
        {
            args.zed = Some("/app/libexec/zed-editor".into());
            unsafe { env::set_var("ZED_UPDATE_EXPLANATION", "Please use flatpak to update zed") };
        }
        args
    }

    fn get_flatpak_dir() -> Option<PathBuf> {
        if env::var(NO_ESCAPE_ENV_NAME).is_ok() {
            return None;
        }

        if let Ok(flatpak_id) = env::var("FLATPAK_ID") {
            if !flatpak_id.starts_with("dev.zed.Zed") {
                return None;
            }

            let install_dir = Command::new("/usr/bin/flatpak-spawn")
                .arg("--host")
                .arg("flatpak")
                .arg("info")
                .arg("--show-location")
                .arg(flatpak_id)
                .output()
                .unwrap();
            let install_dir = PathBuf::from(String::from_utf8(install_dir.stdout).unwrap().trim());
            Some(install_dir.join("files"))
        } else {
            None
        }
    }

    fn get_xdg_env_args() -> Vec<OsString> {
        let xdg_keys = [
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ];
        env::vars()
            .filter(|(key, _)| xdg_keys.contains(&key.as_str()))
            .map(|(key, val)| format!("--env=FLATPAK_{}={}", key, val).into())
            .collect()
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use anyhow::Context;
    use release_channel::app_identifier;
    use windows::{
        Win32::{
            Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GENERIC_WRITE, GetLastError},
            Storage::FileSystem::{
                CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING, WriteFile,
            },
            System::Threading::CreateMutexW,
        },
        core::HSTRING,
    };

    use crate::{Detect, InstalledApp};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{ExitStatus, Stdio};

    fn check_single_instance() -> bool {
        let mutex = unsafe {
            CreateMutexW(
                None,
                false,
                &HSTRING::from(format!("{}-Instance-Mutex", app_identifier())),
            )
            .expect("Unable to create instance sync event")
        };
        let last_err = unsafe { GetLastError() };
        let _ = unsafe { CloseHandle(mutex) };
        last_err != ERROR_ALREADY_EXISTS
    }

    struct App(PathBuf);

    impl InstalledApp for App {
        fn zed_version_string(&self) -> String {
            format!(
                "Zed {}{}{} – {}",
                if *release_channel::RELEASE_CHANNEL_NAME == "stable" {
                    "".to_string()
                } else {
                    format!("{} ", *release_channel::RELEASE_CHANNEL_NAME)
                },
                option_env!("RELEASE_VERSION").unwrap_or_default(),
                match option_env!("ZED_COMMIT_SHA") {
                    Some(commit_sha) => format!(" {commit_sha} "),
                    None => "".to_string(),
                },
                self.0.display(),
            )
        }

        fn launch(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
            _launch_lane: &str,
        ) -> anyhow::Result<()> {
            if check_single_instance() {
                let mut cmd = std::process::Command::new(self.0.clone());
                cmd.arg(ipc_url);
                if let Some(dir) = user_data_dir {
                    cmd.arg("--user-data-dir").arg(dir);
                }
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                cmd.spawn()?;
            } else {
                unsafe {
                    let pipe = CreateFileW(
                        &HSTRING::from(format!("\\\\.\\pipe\\{}-Named-Pipe", app_identifier())),
                        GENERIC_WRITE.0,
                        FILE_SHARE_MODE::default(),
                        None,
                        OPEN_EXISTING,
                        FILE_FLAGS_AND_ATTRIBUTES::default(),
                        None,
                    )?;
                    let message = ipc_url.as_bytes();
                    let mut bytes_written = 0;
                    WriteFile(pipe, Some(message), Some(&mut bytes_written), None)?;
                    CloseHandle(pipe)?;
                }
            }
            Ok(())
        }

        fn run_foreground(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
            _launch_lane: &str,
        ) -> io::Result<ExitStatus> {
            let mut cmd = std::process::Command::new(self.0.clone());
            cmd.arg(ipc_url).arg("--foreground");
            if let Some(dir) = user_data_dir {
                cmd.arg("--user-data-dir").arg(dir);
            }
            cmd.spawn()?.wait()
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Detect {
        pub fn detect(path: Option<&Path>) -> anyhow::Result<impl InstalledApp> {
            let path = if let Some(path) = path {
                path.to_path_buf().canonicalize()?
            } else {
                let cli = std::env::current_exe()?;
                let dir = cli.parent().context("no parent path for cli")?;

                // ../Zed.exe is the standard, lib/zed is for MSYS2, ./zed.exe is for the target
                // directory in development builds.
                let possible_locations = ["../Zed.exe", "../lib/zed/zed-editor.exe", "./zed.exe"];
                possible_locations
                    .iter()
                    .find_map(|p| dir.join(p).canonicalize().ok().filter(|path| path != &cli))
                    .context(format!(
                        "could not find any of: {}",
                        possible_locations.join(", ")
                    ))?
            };

            Ok(App(path))
        }
    }
}

#[cfg(target_os = "macos")]
mod mac_os {
    use anyhow::{Context as _, Result};
    use core_foundation::{
        array::{CFArray, CFIndex},
        base::TCFType as _,
        string::kCFStringEncodingUTF8,
        url::{CFURL, CFURLCreateWithBytes},
    };
    use core_services::{
        LSLaunchURLSpec, LSOpenFromURLSpec, kLSLaunchDefaults, kLSLaunchDontSwitch,
    };
    use serde::Deserialize;
    use std::{
        ffi::OsStr,
        fs, io,
        os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
        path::{Path, PathBuf},
        process::{Command, ExitStatus},
        ptr,
    };

    use cli::FORCE_CLI_MODE_ENV_VAR_NAME;

    use crate::{Detect, InstalledApp};

    #[derive(Debug, Deserialize)]
    struct InfoPlist {
        #[serde(rename = "CFBundleShortVersionString")]
        bundle_short_version_string: String,
        #[serde(rename = "CFBundleVersion")]
        bundle_version: String,
        #[serde(rename = "CFBundleExecutable")]
        bundle_executable: String,
        #[serde(default, rename = "ZedCliLaunchExecutableDirectly")]
        launch_executable_directly: bool,
    }

    enum Bundle {
        App {
            app_bundle: PathBuf,
            plist: InfoPlist,
        },
        LocalPath {
            executable: PathBuf,
        },
    }

    fn open_secure_launch_log(log_path: &Path) -> Option<(fs::File, fs::File)> {
        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(log_path)
            .ok()?;
        fs::set_permissions(log_path, fs::Permissions::from_mode(0o600)).ok()?;
        let stderr = stdout.try_clone().ok()?;
        Some((stdout, stderr))
    }

    fn locate_bundle() -> Result<PathBuf> {
        let cli_path = std::env::current_exe()?.canonicalize()?;
        let mut app_path = cli_path.clone();
        while app_path.extension() != Some(OsStr::new("app")) {
            anyhow::ensure!(
                app_path.pop(),
                "cannot find app bundle containing {cli_path:?}"
            );
        }
        Ok(app_path)
    }

    impl Detect {
        pub fn detect(path: Option<&Path>) -> anyhow::Result<impl InstalledApp> {
            let bundle_path = if let Some(bundle_path) = path {
                bundle_path
                    .canonicalize()
                    .with_context(|| format!("Args bundle path {bundle_path:?} canonicalization"))?
            } else {
                locate_bundle().context("bundle autodiscovery")?
            };

            match bundle_path.extension().and_then(|ext| ext.to_str()) {
                Some("app") => {
                    let plist_path = bundle_path.join("Contents/Info.plist");
                    let plist =
                        plist::from_file::<_, InfoPlist>(&plist_path).with_context(|| {
                            format!("Reading *.app bundle plist file at {plist_path:?}")
                        })?;
                    Ok(Bundle::App {
                        app_bundle: bundle_path,
                        plist,
                    })
                }
                _ => Ok(Bundle::LocalPath {
                    executable: bundle_path,
                }),
            }
        }
    }

    impl InstalledApp for Bundle {
        fn zed_version_string(&self) -> String {
            format!("Zed {} – {}", self.version(), self.display_path().display(),)
        }

        fn launch(
            &self,
            url: String,
            user_data_dir: Option<&str>,
            launch_lane: &str,
        ) -> anyhow::Result<()> {
            match self {
                Self::App {
                    plist:
                        InfoPlist {
                            launch_executable_directly: true,
                            ..
                        },
                    ..
                } => {
                    let logs_dir = paths::logs_dir();
                    // Diagnostic logging is deliberately fail-open: an unavailable log
                    // directory must never keep the editor from launching.
                    let _ = fs::create_dir_all(logs_dir);
                    self.launch_executable(
                        url,
                        user_data_dir,
                        launch_lane,
                        logs_dir.join("zed-cli.log"),
                    )?;
                }

                Self::App { app_bundle, .. } => {
                    let app_path = app_bundle;

                    let status = unsafe {
                        let app_url = CFURL::from_path(app_path, true)
                            .with_context(|| format!("invalid app path {app_path:?}"))?;
                        let url_to_open = CFURL::wrap_under_create_rule(CFURLCreateWithBytes(
                            ptr::null(),
                            url.as_ptr(),
                            url.len() as CFIndex,
                            kCFStringEncodingUTF8,
                            ptr::null(),
                        ));
                        // equivalent to: open zed-cli:... -a /Applications/Zed\ Preview.app
                        let urls_to_open =
                            CFArray::from_copyable(&[url_to_open.as_concrete_TypeRef()]);
                        LSOpenFromURLSpec(
                            &LSLaunchURLSpec {
                                appURL: app_url.as_concrete_TypeRef(),
                                itemURLs: urls_to_open.as_concrete_TypeRef(),
                                passThruParams: ptr::null(),
                                launchFlags: kLSLaunchDefaults | kLSLaunchDontSwitch,
                                asyncRefCon: ptr::null_mut(),
                            },
                            ptr::null_mut(),
                        )
                    };

                    anyhow::ensure!(
                        status == 0,
                        "cannot start app bundle {}",
                        self.zed_version_string()
                    );
                }

                Self::LocalPath { executable, .. } => {
                    let executable_parent = executable
                        .parent()
                        .with_context(|| format!("Executable {executable:?} path has no parent"))?;
                    self.launch_executable(
                        url,
                        user_data_dir,
                        launch_lane,
                        executable_parent.join("zed_dev.log"),
                    )?;
                }
            }

            Ok(())
        }

        fn record_cli_launch_failure(&self, failure_class: &str, lane: &str) {
            let telemetry_disabled =
                std::env::var("ZED_10X_TELEMETRY_DISABLED").is_ok_and(|value| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                });
            for node_path in [
                Path::new("/opt/homebrew/bin/node"),
                Path::new("/usr/local/bin/node"),
            ] {
                if node_path.is_file() {
                    if self
                        .record_cli_launch_failure_with_node(
                            node_path,
                            failure_class,
                            lane,
                            telemetry_disabled,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
            }
        }

        fn run_foreground(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
            launch_lane: &str,
        ) -> io::Result<ExitStatus> {
            let mut cmd = std::process::Command::new(self.executable_path());
            cmd.env("ZED_10X_LAUNCH_LANE", launch_lane).arg(ipc_url);
            if let Some(dir) = user_data_dir {
                cmd.arg("--user-data-dir").arg(dir);
            }
            cmd.status()
        }

        fn path(&self) -> PathBuf {
            self.executable_path()
        }
    }

    impl Bundle {
        fn version(&self) -> String {
            match self {
                Self::App { plist, .. } => plist.bundle_short_version_string.clone(),
                Self::LocalPath { .. } => "<development>".to_string(),
            }
        }

        fn display_path(&self) -> &Path {
            match self {
                Self::App { app_bundle, .. } => app_bundle,
                Self::LocalPath { executable, .. } => executable,
            }
        }

        fn executable_path(&self) -> PathBuf {
            match self {
                Self::App { app_bundle, plist } => app_bundle
                    .join("Contents/MacOS")
                    .join(&plist.bundle_executable),
                Self::LocalPath { executable, .. } => executable.clone(),
            }
        }

        fn launch_executable(
            &self,
            url: String,
            user_data_dir: Option<&str>,
            launch_lane: &str,
            log_path: PathBuf,
        ) -> anyhow::Result<()> {
            let log_files = open_secure_launch_log(&log_path);
            let executable = self.executable_path();
            let mut command = std::process::Command::new(&executable);
            command
                .env(FORCE_CLI_MODE_ENV_VAR_NAME, "")
                .env("ZED_10X_LAUNCH_LANE", launch_lane);
            if let Some(dir) = user_data_dir {
                command.arg("--user-data-dir").arg(dir);
            }
            if let Some((subprocess_stdout_file, subprocess_stderr_file)) = log_files {
                command
                    .stderr(subprocess_stderr_file)
                    .stdout(subprocess_stdout_file);
            } else {
                command
                    .stderr(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null());
            }
            command.arg(url);

            command
                .spawn()
                .with_context(|| format!("Spawning bundle executable {executable:?}"))?;
            Ok(())
        }

        fn record_cli_launch_failure_with_node(
            &self,
            node_path: &Path,
            failure_class: &str,
            lane: &str,
            telemetry_disabled: bool,
        ) -> anyhow::Result<()> {
            let (
                app_bundle,
                InfoPlist {
                    bundle_short_version_string,
                    bundle_version,
                    launch_executable_directly,
                    ..
                },
            ) = match self {
                Self::App { app_bundle, plist } => (app_bundle, plist),
                Self::LocalPath { .. } => return Ok(()),
            };
            if !launch_executable_directly {
                return Ok(());
            }

            let collector = app_bundle
                .join("Contents/Resources")
                .join("zed-10x-canary.mjs");
            if !collector.is_file() {
                return Ok(());
            }

            let store_dir = std::env::var_os("ZED_10X_CANARY_STORE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    paths::home_dir().join("Library/Application Support/Zed 10x/dogfood-canary")
                });
            let mut command = std::process::Command::new(node_path);
            command
                .env_clear()
                .env("HOME", paths::home_dir())
                .env(
                    "PATH",
                    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                )
                .arg(collector)
                .args([
                    "record",
                    "cli.launch.failure",
                    "--cohort",
                    "zed10x",
                    "--lane",
                    lane,
                    "--app-version",
                    bundle_short_version_string,
                    "--build-version",
                    bundle_version,
                    "--failure-class",
                    failure_class,
                    "--session-kind",
                    "cli",
                ])
                .arg("--store")
                .arg(store_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if telemetry_disabled {
                command.env("ZED_10X_TELEMETRY_DISABLED", "1");
            }
            command
                .spawn()
                .context("Starting fail-open CLI failure telemetry")?;
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Duration;

        #[test]
        fn custom_bundle_executable_is_used_for_direct_launch_contract() {
            let bundle = Bundle::App {
                app_bundle: PathBuf::from("/Applications/Zed 10x.app"),
                plist: InfoPlist {
                    bundle_short_version_string: "0.1.0".to_string(),
                    bundle_version: "1".to_string(),
                    bundle_executable: "zed-10x-launcher".to_string(),
                    launch_executable_directly: true,
                },
            };

            assert_eq!(
                bundle.executable_path(),
                PathBuf::from("/Applications/Zed 10x.app/Contents/MacOS/zed-10x-launcher")
            );
            assert!(matches!(
                bundle,
                Bundle::App {
                    plist: InfoPlist {
                        launch_executable_directly: true,
                        ..
                    },
                    ..
                }
            ));
        }

        #[test]
        fn direct_launch_passes_the_preclassified_lane_to_the_launcher() {
            let temp_dir = tempfile::tempdir().unwrap();
            let lane_evidence = temp_dir.path().join("lane.txt");
            let executable = temp_dir.path().join("launcher");
            fs::write(
                &executable,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$ZED_10X_LAUNCH_LANE\" > '{}'\n",
                    lane_evidence.display(),
                ),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let bundle = Bundle::LocalPath { executable };
            let log_path = temp_dir.path().join("launcher.log");

            bundle
                .launch_executable(
                    "zed-cli://fixture".to_string(),
                    None,
                    "intrepid",
                    log_path.clone(),
                )
                .unwrap();
            for _ in 0..100 {
                if lane_evidence.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(fs::read_to_string(lane_evidence).unwrap(), "intrepid\n");
            assert_eq!(
                fs::metadata(log_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        #[test]
        fn direct_launch_is_fail_open_when_the_diagnostic_log_is_unavailable() {
            let temp_dir = tempfile::tempdir().unwrap();
            let launch_evidence = temp_dir.path().join("launched.txt");
            let executable = temp_dir.path().join("launcher");
            fs::write(
                &executable,
                format!(
                    "#!/bin/sh\nprintf 'launched\\n' > '{}'\n",
                    launch_evidence.display(),
                ),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let bundle = Bundle::LocalPath { executable };
            let unavailable_log = temp_dir.path().join("missing").join("launcher.log");

            bundle
                .launch_executable(
                    "zed-cli://fixture".to_string(),
                    None,
                    "local",
                    unavailable_log,
                )
                .unwrap();
            for _ in 0..100 {
                if launch_evidence.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                fs::read_to_string(launch_evidence).unwrap(),
                "launched\n",
                "diagnostic-log failure must not block the editor launcher"
            );
        }

        #[test]
        fn direct_bundle_records_a_sanitized_content_free_cli_failure() {
            let temp_dir = tempfile::tempdir().unwrap();
            let app_bundle = temp_dir.path().join("Zed 10x.app");
            let resources = app_bundle.join("Contents/Resources");
            fs::create_dir_all(&resources).unwrap();
            let collector = resources.join("zed-10x-canary.mjs");
            fs::write(&collector, "").unwrap();

            let evidence_path = temp_dir.path().join("collector-arguments.txt");
            let fake_node = temp_dir.path().join("fake-node");
            fs::write(
                &fake_node,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'github=%s\\n' \"${{GITHUB_TOKEN+set}}\" >> '{}'\n",
                    evidence_path.display(),
                    evidence_path.display(),
                ),
            )
            .unwrap();
            fs::set_permissions(&fake_node, fs::Permissions::from_mode(0o755)).unwrap();

            let bundle = Bundle::App {
                app_bundle,
                plist: InfoPlist {
                    bundle_short_version_string: "1.13.0-10x".to_string(),
                    bundle_version: "20260727.1".to_string(),
                    bundle_executable: "zed-10x-launcher".to_string(),
                    launch_executable_directly: true,
                },
            };
            bundle
                .record_cli_launch_failure_with_node(
                    &fake_node,
                    "handshake_timeout",
                    "local",
                    false,
                )
                .unwrap();

            for _ in 0..100 {
                if evidence_path.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let evidence = fs::read_to_string(evidence_path).unwrap();
            assert!(evidence.contains("cli.launch.failure"));
            assert!(evidence.contains("handshake_timeout"));
            assert!(evidence.contains("20260727.1"));
            assert!(evidence.contains("github=\n"));
            assert!(!evidence.contains("GITHUB_TOKEN"));
        }
    }

    pub(super) fn spawn_channel_cli(
        channel: release_channel::ReleaseChannel,
        leftover_args: Vec<String>,
    ) -> Result<()> {
        use anyhow::bail;

        let app_path_prompt = format!(
            "POSIX path of (path to application \"{}\")",
            channel.display_name()
        );
        let app_path_output = Command::new("osascript")
            .arg("-e")
            .arg(&app_path_prompt)
            .output()?;
        if !app_path_output.status.success() {
            bail!(
                "Could not determine app path for {}",
                channel.display_name()
            );
        }
        let app_path = String::from_utf8(app_path_output.stdout)?.trim().to_owned();
        let cli_path = format!("{app_path}/Contents/MacOS/cli");
        Command::new(cli_path).args(leftover_args).spawn()?;
        Ok(())
    }
}
