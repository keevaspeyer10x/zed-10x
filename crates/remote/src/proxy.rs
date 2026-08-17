use thiserror::Error;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProxyMode {
    Start,
    Reconnect,
    ReconnectOrStart,
}

impl ProxyMode {
    pub fn append_cli_args(self, args: &mut Vec<String>) {
        match self {
            Self::Start => {}
            Self::Reconnect => args.push("--reconnect".to_owned()),
            Self::ReconnectOrStart => args.push("--reconnect-or-start".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_modes_have_distinct_command_line_contracts() {
        for (mode, expected) in [
            (ProxyMode::Start, vec![]),
            (ProxyMode::Reconnect, vec!["--reconnect"]),
            (ProxyMode::ReconnectOrStart, vec!["--reconnect-or-start"]),
        ] {
            let mut args = Vec::new();
            mode.append_cli_args(&mut args);
            assert_eq!(args, expected);
        }
    }
}

#[derive(Copy, Clone, Error, Debug)]
#[repr(i32)]
pub enum ProxyLaunchError {
    // We're using 90 as the exit code, because 0-78 are often taken
    // by shells and other conventions and >128 also has certain meanings
    // in certain contexts.
    #[error("Attempted reconnect, but server not running.")]
    ServerNotRunning = 90,
}

impl ProxyLaunchError {
    pub fn to_exit_code(self) -> i32 {
        self as i32
    }

    pub fn from_exit_code(exit_code: i32) -> Option<Self> {
        match exit_code {
            90 => Some(Self::ServerNotRunning),
            _ => None,
        }
    }
}
