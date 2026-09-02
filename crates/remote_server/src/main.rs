use clap::Parser;
use remote_server::Commands;
use std::io::Write as _;
use std::path::PathBuf;

#[derive(Parser)]
#[command(disable_version_flag = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Used for SSH/Git password authentication, to remove the need for netcat as a dependency,
    /// by having Zed act like netcat communicating over a Unix socket.
    #[arg(long, hide = true)]
    askpass: Option<String>,
    /// Used for recording minidumps on crashes by having the server run a separate
    /// process communicating over a socket.
    #[arg(long, hide = true)]
    crash_handler: Option<PathBuf>,
    /// Used for loading the environment from the project.
    #[arg(long, hide = true)]
    printenv: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if let Some(socket_path) = &cli.askpass {
        askpass::main(socket_path);
        return Ok(());
    }

    if let Some(socket) = &cli.crash_handler {
        crashes::crash_server(socket.as_path(), paths::logs_dir().clone());
        return Ok(());
    }

    if cli.printenv {
        util::shell_env::print_env();
        return Ok(());
    }

    if let Some(command) = cli.command {
        use remote_server::ExecuteProxyError;

        let res = remote_server::run(command);
        #[cfg(unix)]
        if let Err(error) = &res
            && let Some(error) = error.downcast_ref::<remote_server::EnvExecExitError>()
        {
            std::io::stderr()
                .write_fmt(format_args!("{error:#}\n"))
                .ok();
            if let Some(signal) = error.signal() {
                // Preserve the command's signal identity after all owned
                // descendants and forwarding threads have been cleaned up.
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                    libc::raise(signal);
                }
            }
            std::process::exit(error.to_exit_code());
        }
        if let Err(e) = &res
            && let Some(e) = e.downcast_ref::<ExecuteProxyError>()
        {
            std::io::stderr().write_fmt(format_args!("{e:#}\n")).ok();
            // It is important for us to report the proxy spawn exit code here
            // instead of the generic 1 that result returns
            // The client reads the exit code to determine if the server process has died when trying to reconnect
            // signaling that it needs to try spawning a new server
            std::process::exit(e.to_exit_code());
        }
        res
    } else {
        std::io::stderr()
            .write_all(b"usage: remote <run|proxy|version>\n")
            .ok();
        std::process::exit(1);
    }
}
