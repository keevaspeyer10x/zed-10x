use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use futures::io::{BufReader, BufWriter};
use futures::{
    AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, Stream, StreamExt as _,
};
use gpui::AsyncApp;

use util::TryFutureExt as _;
use util::process::Child;
use util::shell::Shell;
use util::shell_builder::ShellBuilder;

use crate::client::ModelContextServerBinary;
use crate::transport::Transport;

pub struct StdioTransport {
    stdout_sender: async_channel::Sender<String>,
    stdin_receiver: async_channel::Receiver<String>,
    stderr_receiver: async_channel::Receiver<String>,
    server: Child,
}

impl StdioTransport {
    pub fn new(
        binary: ModelContextServerBinary,
        working_directory: &Option<PathBuf>,
        cx: &AsyncApp,
    ) -> Result<Self> {
        let builder = ShellBuilder::new(&Shell::System, cfg!(windows)).non_interactive();
        let mut command =
            builder.build_std_command(Some(binary.executable.display().to_string()), &binary.args);

        command.envs(binary.env.unwrap_or_default());

        if let Some(working_directory) = working_directory {
            command.current_dir(working_directory);
        }

        let mut server = Child::spawn(
            command,
            std::process::Stdio::piped(),
            std::process::Stdio::piped(),
            std::process::Stdio::piped(),
        )?;

        let stdin = server.stdin.take().unwrap();
        let stdout = server.stdout.take().unwrap();
        let stderr = server.stderr.take().unwrap();

        let (stdin_sender, stdin_receiver) = async_channel::unbounded::<String>();
        let (stdout_sender, stdout_receiver) = async_channel::unbounded::<String>();
        let (stderr_sender, stderr_receiver) = async_channel::unbounded::<String>();

        cx.spawn(async move |_| {
            Self::handle_output(stdin, binary.stdin_prelude, stdout_receiver)
                .log_err()
                .await
        })
        .detach();

        cx.spawn(async move |_| Self::handle_input(stdout, stdin_sender).await)
            .detach();

        cx.spawn(async move |_| Self::handle_err(stderr, stderr_sender).await)
            .detach();

        Ok(Self {
            stdout_sender,
            stdin_receiver,
            stderr_receiver,
            server,
        })
    }

    async fn handle_input<Stdout>(stdin: Stdout, inbound_rx: async_channel::Sender<String>)
    where
        Stdout: AsyncRead + Unpin + Send + 'static,
    {
        let mut stdin = BufReader::new(stdin);
        let mut line = String::new();
        while let Ok(n) = stdin.read_line(&mut line).await {
            if n == 0 {
                break;
            }
            if inbound_rx.send(line.clone()).await.is_err() {
                break;
            }
            line.clear();
        }
    }

    async fn handle_output<Stdin>(
        stdin: Stdin,
        stdin_prelude: Vec<u8>,
        outbound_rx: async_channel::Receiver<String>,
    ) -> Result<()>
    where
        Stdin: AsyncWrite + Unpin + Send + 'static,
    {
        let mut stdin = BufWriter::new(stdin);
        if !stdin_prelude.is_empty() {
            stdin.write_all(&stdin_prelude).await?;
            stdin.flush().await?;
        }
        let mut pinned_rx = Box::pin(outbound_rx);
        while let Some(message) = pinned_rx.next().await {
            log::trace!("outgoing message: {}", message);

            stdin.write_all(message.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }
        Ok(())
    }

    async fn handle_err<Stderr>(stderr: Stderr, stderr_tx: async_channel::Sender<String>)
    where
        Stderr: AsyncRead + Unpin + Send + 'static,
    {
        let mut stderr = BufReader::new(stderr);
        let mut line = String::new();
        while let Ok(n) = stderr.read_line(&mut line).await {
            if n == 0 {
                break;
            }
            if stderr_tx.send(line.clone()).await.is_err() {
                break;
            }
            line.clear();
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, message: String) -> Result<()> {
        Ok(self.stdout_sender.send(message).await?)
    }

    fn receive(&self) -> Pin<Box<dyn Stream<Item = String> + Send>> {
        Box::pin(self.stdin_receiver.clone())
    }

    fn receive_err(&self) -> Pin<Box<dyn Stream<Item = String> + Send>> {
        Box::pin(self.stderr_receiver.clone())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.server.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::{
        sync::Arc,
        task::{Context, Poll},
    };

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl AsyncWrite for SharedWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.0.lock().extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn stdin_prelude_precedes_first_protocol_message() -> Result<()> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sender, receiver) = async_channel::unbounded();
        sender.try_send("first-message".to_string())?;
        drop(sender);

        pollster::block_on(StdioTransport::handle_output(
            SharedWriter(bytes.clone()),
            b"private-prelude".to_vec(),
            receiver,
        ))?;

        assert_eq!(&*bytes.lock(), b"private-preludefirst-message\n");
        Ok(())
    }
}
