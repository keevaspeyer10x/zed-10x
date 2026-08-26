use std::{
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use anyhow::{Context, Result};
use async_compression::futures::bufread::{BzDecoder, GzipDecoder};
use futures::{AsyncRead, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt, io::BufReader};
use sha2::{Digest, Sha256};

use crate::{HttpClient, github::AssetKind};

fn sha256_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub struct GithubBinaryMetadata {
    pub metadata_version: u64,
    pub digest: Option<String>,
}

impl GithubBinaryMetadata {
    pub async fn read_from_file(metadata_path: &Path) -> Result<GithubBinaryMetadata> {
        let metadata_content = async_fs::read_to_string(metadata_path)
            .await
            .with_context(|| format!("reading metadata file at {metadata_path:?}"))?;
        serde_json::from_str(&metadata_content)
            .with_context(|| format!("parsing metadata file at {metadata_path:?}"))
    }

    pub async fn write_to_file(&self, metadata_path: &Path) -> Result<()> {
        let metadata_content = serde_json::to_string(self)
            .with_context(|| format!("serializing metadata for {metadata_path:?}"))?;
        async_fs::write(metadata_path, metadata_content.as_bytes())
            .await
            .with_context(|| format!("writing metadata file at {metadata_path:?}"))?;
        Ok(())
    }
}

pub async fn download_server_binary(
    http_client: &dyn HttpClient,
    url: &str,
    digest: Option<&str>,
    destination_path: &Path,
    asset_kind: AssetKind,
) -> Result<(), anyhow::Error> {
    log::info!("downloading github artifact from {url}");
    let Some(destination_parent) = destination_path.parent() else {
        anyhow::bail!("destination path has no parent: {destination_path:?}");
    };

    let staging_path = staging_path(destination_parent, asset_kind)?;
    let mut response = http_client
        .get(url, Default::default(), true)
        .await
        .with_context(|| format!("downloading release from {url}"))?;
    let body = response.body_mut();

    if let Err(err) = extract_to_staging(body, digest, url, &staging_path, asset_kind).await {
        cleanup_staging_path(&staging_path, asset_kind).await;
        return Err(err);
    }

    if let Err(err) = finalize_download(&staging_path, destination_path).await {
        cleanup_staging_path(&staging_path, asset_kind).await;
        return Err(err);
    }

    Ok(())
}

pub async fn download_server_raw_binary(
    http_client: &dyn HttpClient,
    url: &str,
    digest: Option<&str>,
    destination_path: &Path,
    binary_file_name: &str,
) -> Result<(), anyhow::Error> {
    log::info!("downloading raw binary from {url}");
    let Some(destination_parent) = destination_path.parent() else {
        anyhow::bail!("destination path has no parent: {destination_path:?}");
    };

    let staging_path = staging_dir_path(destination_parent)?;
    let result = async {
        let mut response = http_client
            .get(url, Default::default(), true)
            .await
            .with_context(|| format!("downloading release from {url}"))?;

        let binary_path = staging_path.join(binary_file_name);
        let mut writer = HashingWriter {
            writer: async_fs::File::create(&binary_path)
                .await
                .with_context(|| format!("creating a file {binary_path:?} for {url}"))?,
            hasher: Sha256::new(),
        };
        futures::io::copy(&mut BufReader::new(response.body_mut()), &mut writer)
            .await
            .with_context(|| format!("saving binary contents from {url}"))?;
        let asset_sha_256 = writer
            .finish()
            .await
            .with_context(|| format!("flushing binary contents for {url}"))?;

        if let Some(expected_sha_256) = digest {
            anyhow::ensure!(
                sha256_matches(&asset_sha_256, expected_sha_256),
                "{url} asset got SHA-256 mismatch. Expected: {expected_sha_256}, Got: {asset_sha_256}",
            );
        }

        util::fs::make_file_executable(&binary_path)
            .await
            .with_context(|| format!("marking {binary_path:?} as executable"))?;
        finalize_download(&staging_path, destination_path).await
    }
    .await;

    if let Err(err) = result {
        if let Err(err) = async_fs::remove_dir_all(&staging_path).await {
            log::warn!("failed to remove staging directory {staging_path:?}: {err:?}");
        }
        return Err(err);
    }

    Ok(())
}

async fn extract_to_staging(
    body: impl AsyncRead + Unpin,
    digest: Option<&str>,
    url: &str,
    staging_path: &Path,
    asset_kind: AssetKind,
) -> Result<()> {
    match digest {
        Some(expected_sha_256) => {
            let temp_asset_file = tempfile::NamedTempFile::new()
                .with_context(|| format!("creating a temporary file for {url}"))?;
            let (temp_asset_file, _temp_guard) = temp_asset_file.into_parts();
            let mut writer = HashingWriter {
                writer: async_fs::File::from(temp_asset_file),
                hasher: Sha256::new(),
            };
            futures::io::copy(&mut BufReader::new(body), &mut writer)
                .await
                .with_context(|| {
                    format!("saving archive contents into the temporary file for {url}")
                })?;
            let asset_sha_256 = format!("{:x}", writer.hasher.finalize());

            anyhow::ensure!(
                sha256_matches(&asset_sha_256, expected_sha_256),
                "{url} asset got SHA-256 mismatch. Expected: {expected_sha_256}, Got: {asset_sha_256}",
            );
            writer
                .writer
                .seek(std::io::SeekFrom::Start(0))
                .await
                .with_context(|| format!("seeking temporary file for {url}"))?;
            stream_file_archive(&mut writer.writer, url, staging_path, asset_kind)
                .await
                .with_context(|| {
                    format!("extracting downloaded asset for {url} into {staging_path:?}")
                })?;
        }
        None => {
            stream_response_archive(body, url, staging_path, asset_kind)
                .await
                .with_context(|| {
                    format!("extracting response for asset {url} into {staging_path:?}")
                })?;
        }
    }
    Ok(())
}

fn staging_dir_path(parent: &Path) -> Result<PathBuf> {
    let dir = tempfile::Builder::new()
        .prefix(".tmp-github-download-")
        .tempdir_in(parent)
        .with_context(|| format!("creating staging directory in {parent:?}"))?;
    Ok(dir.keep())
}

fn staging_path(parent: &Path, asset_kind: AssetKind) -> Result<PathBuf> {
    match asset_kind {
        AssetKind::TarGz | AssetKind::TarBz2 | AssetKind::Zip => staging_dir_path(parent),
        AssetKind::Gz => {
            let path = tempfile::Builder::new()
                .prefix(".tmp-github-download-")
                .tempfile_in(parent)
                .with_context(|| format!("creating staging file in {parent:?}"))?
                .into_temp_path()
                .keep()
                .with_context(|| format!("persisting staging file in {parent:?}"))?;
            Ok(path)
        }
    }
}

async fn cleanup_staging_path(staging_path: &Path, asset_kind: AssetKind) {
    match asset_kind {
        AssetKind::TarGz | AssetKind::TarBz2 | AssetKind::Zip => {
            if let Err(err) = async_fs::remove_dir_all(staging_path).await {
                log::warn!("failed to remove staging directory {staging_path:?}: {err:?}");
            }
        }
        AssetKind::Gz => {
            if let Err(err) = async_fs::remove_file(staging_path).await {
                log::warn!("failed to remove staging file {staging_path:?}: {err:?}");
            }
        }
    }
}

async fn finalize_download(staging_path: &Path, destination_path: &Path) -> Result<()> {
    _ = async_fs::remove_dir_all(destination_path).await;
    async_fs::rename(staging_path, destination_path)
        .await
        .with_context(|| format!("renaming {staging_path:?} to {destination_path:?}"))?;
    Ok(())
}

async fn stream_response_archive(
    response: impl AsyncRead + Unpin,
    url: &str,
    destination_path: &Path,
    asset_kind: AssetKind,
) -> Result<()> {
    match asset_kind {
        AssetKind::TarGz => extract_tar_gz(destination_path, url, response).await?,
        AssetKind::TarBz2 => extract_tar_bz2(destination_path, url, response).await?,
        AssetKind::Gz => extract_gz(destination_path, url, response).await?,
        AssetKind::Zip => {
            util::archive::extract_zip(destination_path, response).await?;
        }
    };
    Ok(())
}

async fn stream_file_archive(
    file_archive: impl AsyncRead + AsyncSeek + Unpin,
    url: &str,
    destination_path: &Path,
    asset_kind: AssetKind,
) -> Result<()> {
    match asset_kind {
        AssetKind::TarGz => extract_tar_gz(destination_path, url, file_archive).await?,
        AssetKind::TarBz2 => extract_tar_bz2(destination_path, url, file_archive).await?,
        AssetKind::Gz => extract_gz(destination_path, url, file_archive).await?,
        #[cfg(not(windows))]
        AssetKind::Zip => {
            util::archive::extract_seekable_zip(destination_path, file_archive).await?;
        }
        #[cfg(windows)]
        AssetKind::Zip => {
            util::archive::extract_zip(destination_path, file_archive).await?;
        }
    };
    Ok(())
}

async fn extract_tar_gz(
    destination_path: &Path,
    url: &str,
    from: impl AsyncRead + Unpin,
) -> Result<(), anyhow::Error> {
    let decompressed_bytes = GzipDecoder::new(BufReader::new(from));
    unpack_tar_archive(destination_path, url, decompressed_bytes).await?;
    Ok(())
}

async fn extract_tar_bz2(
    destination_path: &Path,
    url: &str,
    from: impl AsyncRead + Unpin,
) -> Result<(), anyhow::Error> {
    let decompressed_bytes = BzDecoder::new(BufReader::new(from));
    unpack_tar_archive(destination_path, url, decompressed_bytes).await?;
    Ok(())
}

async fn unpack_tar_archive(
    destination_path: &Path,
    url: &str,
    archive_bytes: impl AsyncRead + Unpin,
) -> Result<(), anyhow::Error> {
    // We don't need to set the modified time. It's irrelevant to downloaded
    // archive verification, and some filesystems return errors when asked to
    // apply it after extraction.
    let archive = async_tar::ArchiveBuilder::new(PendingSafeReader::new(archive_bytes))
        .set_preserve_mtime(false)
        .build();
    archive
        .unpack(&destination_path)
        .await
        .with_context(|| format!("extracting {url} to {destination_path:?}"))?;
    Ok(())
}

/// Prevents a consumer from losing bytes when it does not retain a partial read
/// across `Poll::Pending`. The buffer is bounded by the consumer's current read
/// request and the archive remains fully streaming.
struct PendingSafeReader<R> {
    reader: R,
    buffered: Vec<u8>,
    reached_eof: bool,
}

impl<R> PendingSafeReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buffered: Vec::new(),
            reached_eof: false,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for PendingSafeReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        output: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if output.is_empty() {
            return Poll::Ready(Ok(0));
        }

        while self.buffered.len() < output.len() && !self.reached_eof {
            let mut scratch = [0_u8; 8 * 1024];
            let remaining = output.len() - self.buffered.len();
            let read_length = remaining.min(scratch.len());
            match Pin::new(&mut self.reader).poll_read(cx, &mut scratch[..read_length]) {
                Poll::Ready(Ok(0)) => self.reached_eof = true,
                Poll::Ready(Ok(read)) => self.buffered.extend_from_slice(&scratch[..read]),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }

        let read_length = output.len().min(self.buffered.len());
        output[..read_length].copy_from_slice(&self.buffered[..read_length]);
        self.buffered.drain(..read_length);
        Poll::Ready(Ok(read_length))
    }
}

async fn extract_gz(
    destination_path: &Path,
    url: &str,
    from: impl AsyncRead + Unpin,
) -> Result<(), anyhow::Error> {
    let mut decompressed_bytes = GzipDecoder::new(BufReader::new(from));
    let mut file = async_fs::File::create(&destination_path)
        .await
        .with_context(|| {
            format!("creating a file {destination_path:?} for a download from {url}")
        })?;
    futures::io::copy(&mut decompressed_bytes, &mut file)
        .await
        .with_context(|| format!("extracting {url} to {destination_path:?}"))?;
    Ok(())
}

struct HashingWriter<W: AsyncWrite + Unpin> {
    writer: W,
    hasher: Sha256,
}

impl<W: AsyncWrite + Unpin> HashingWriter<W> {
    /// Closes and drops the inner writer, returning the hex SHA-256 digest of
    /// everything written.
    ///
    /// Taking `self` by value guarantees the writer is dropped before this
    /// returns. For file writers this releases the OS handle, which Windows
    /// requires before an ancestor directory can be renamed or deleted; note
    /// that closing alone is not enough, as `async_fs::File` holds its handle
    /// until dropped.
    async fn finish(mut self) -> std::io::Result<String> {
        self.writer.close().await?;
        drop(self.writer);
        Ok(format!("{:x}", self.hasher.finalize()))
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for HashingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>> {
        match Pin::new(&mut self.writer).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                self.hasher.update(&buf[..n]);
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        Pin::new(&mut self.writer).poll_close(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AsyncBody, Response};
    use futures::future::BoxFuture;
    use http::HeaderValue;
    use url::Url;

    struct StaticResponseClient {
        body: Vec<u8>,
    }

    struct PendingChunkedReader {
        bytes: Vec<u8>,
        position: usize,
        return_pending: bool,
    }

    impl PendingChunkedReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                position: 0,
                return_pending: true,
            }
        }
    }

    impl AsyncRead for PendingChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buffer: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.return_pending {
                self.return_pending = false;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            self.return_pending = true;

            if self.position == self.bytes.len() {
                return Poll::Ready(Ok(0));
            }

            let read_length = buffer.len().min(1);
            buffer[..read_length]
                .copy_from_slice(&self.bytes[self.position..self.position + read_length]);
            self.position += read_length;
            Poll::Ready(Ok(read_length))
        }
    }

    fn tar_gz_with_pax_value_containing_newlines() -> Vec<u8> {
        vec![
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xed, 0xd3, 0xb1, 0x0a,
            0xc2, 0x30, 0x10, 0x80, 0xe1, 0xcc, 0x7d, 0x8a, 0x3e, 0x41, 0x9a, 0x06, 0xd3, 0x4e,
            0x82, 0xe0, 0x52, 0xc1, 0x41, 0x70, 0x72, 0x8c, 0x18, 0x6c, 0xa1, 0x44, 0x48, 0x23,
            0xad, 0x6f, 0xaf, 0xc5, 0x41, 0x1c, 0x3a, 0x56, 0x50, 0xff, 0x6f, 0xb9, 0xe3, 0x96,
            0x9b, 0x7e, 0x99, 0xc9, 0x6c, 0xb5, 0xb3, 0x43, 0xe5, 0xec, 0xc9, 0x05, 0x31, 0x0b,
            0xf5, 0x34, 0x35, 0x95, 0x32, 0xfa, 0xb5, 0x8f, 0xf7, 0x5c, 0xe9, 0x5c, 0x8b, 0x74,
            0x10, 0x1f, 0x70, 0xed, 0xa2, 0x0d, 0x8f, 0xf7, 0xe2, 0x3f, 0x2d, 0x74, 0xba, 0x5f,
            0x57, 0x9b, 0xed, 0x41, 0x0e, 0x36, 0xc6, 0x20, 0xa3, 0xeb, 0xe2, 0xf2, 0xd8, 0x78,
            0x1b, 0x6e, 0x49, 0xdf, 0xc4, 0x3a, 0xf1, 0xae, 0x6f, 0x1b, 0xef, 0xba, 0x44, 0xe0,
            0x07, 0xd9, 0xb3, 0xf3, 0x71, 0xe6, 0x1f, 0x63, 0xd4, 0xa5, 0x31, 0xd3, 0xfd, 0xab,
            0xe2, 0xbd, 0x7f, 0x55, 0x94, 0x85, 0x11, 0xa9, 0xa2, 0xff, 0xd9, 0xd5, 0xae, 0x6d,
            0x2f, 0xb4, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0xf0, 0xb5, 0xee, 0xb6, 0xeb, 0x7b, 0xe9, 0x00, 0x28, 0x00, 0x00,
        ]
    }

    fn write_octal(field: &mut [u8], value: usize) {
        let value = format!("{:0width$o}\0", value, width = field.len() - 1);
        field.copy_from_slice(value.as_bytes());
    }

    fn append_tar_entry(archive: &mut Vec<u8>, name: &str, entry_type: u8, body: &[u8]) {
        let mut header = [0_u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        write_octal(&mut header[100..108], 0o755);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], body.len());
        write_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = entry_type;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum = header.iter().map(|byte| u32::from(*byte)).sum::<u32>();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        archive.extend_from_slice(&header);
        archive.extend_from_slice(body);
        let padding = (512 - body.len() % 512) % 512;
        archive.resize(archive.len() + padding, 0);
    }

    fn tar_with_large_pax_value() -> Vec<u8> {
        let key = b"SCHILY.xattr.test";
        let value = (0..22_000)
            .map(|index| {
                if index % 97 == 0 {
                    b'\n'
                } else {
                    (index % 251) as u8
                }
            })
            .collect::<Vec<_>>();
        let payload_length = 1 + key.len() + 1 + value.len() + 1;
        let mut record_length = payload_length;
        loop {
            let next_length = payload_length + record_length.to_string().len();
            if next_length == record_length {
                break;
            }
            record_length = next_length;
        }
        let mut pax_body = format!("{record_length} ").into_bytes();
        pax_body.extend_from_slice(key);
        pax_body.push(b'=');
        pax_body.extend_from_slice(&value);
        pax_body.push(b'\n');
        assert_eq!(pax_body.len(), record_length);
        pax_body.extend_from_slice(b"14 path=agent\n");

        let mut archive = Vec::new();
        append_tar_entry(&mut archive, "PaxHeader/agent", b'x', &pax_body);
        append_tar_entry(&mut archive, "fallback", b'0', b"hello\n");
        archive.resize(archive.len() + 1024, 0);
        archive
    }

    impl HttpClient for StaticResponseClient {
        fn send(
            &self,
            _req: http::Request<AsyncBody>,
        ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
            let body = self.body.clone();
            Box::pin(async move {
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from(body))
                    .unwrap())
            })
        }

        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }
    }

    #[test]
    fn downloads_raw_binary_with_uppercase_digest_into_destination_dir() {
        futures::executor::block_on(async {
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");
            let contents = b"#!/bin/sh\necho hello\n".to_vec();
            let expected_sha_256 = format!("{:X}", Sha256::digest(&contents));
            let client = StaticResponseClient { body: contents };

            download_server_raw_binary(
                &client,
                "https://example.com/agent-binary",
                Some(&expected_sha_256),
                &destination_path,
                "agent-binary",
            )
            .await
            .unwrap();

            let binary_path = destination_path.join("agent-binary");
            assert_eq!(
                std::fs::read(&binary_path).unwrap(),
                b"#!/bin/sh\necho hello\n"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&binary_path)
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o111, 0o111, "binary should be executable");
            }
        });
    }

    #[test]
    fn raw_binary_digest_mismatch_cleans_up_staging() {
        futures::executor::block_on(async {
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");
            let client = StaticResponseClient {
                body: b"some binary".to_vec(),
            };

            let error = download_server_raw_binary(
                &client,
                "https://example.com/agent-binary",
                Some("0000000000000000000000000000000000000000000000000000000000000000"),
                &destination_path,
                "agent-binary",
            )
            .await
            .unwrap_err();

            assert!(error.to_string().contains("SHA-256 mismatch"));
            assert!(!destination_path.exists());
            let leftover_entries = std::fs::read_dir(temp_dir.path()).unwrap().count();
            assert_eq!(leftover_entries, 0, "staging directory should be removed");
        });
    }

    #[test]
    fn downloads_archive_with_uppercase_digest_and_extracts_contents() {
        futures::executor::block_on(async {
            let archive = vec![
                0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x21, 0x00,
                0x86, 0xa6, 0x10, 0x36, 0x05, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x05, 0x00,
                0x00, 0x00, 0x61, 0x67, 0x65, 0x6e, 0x74, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x50, 0x4b,
                0x01, 0x02, 0x14, 0x03, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x21, 0x00,
                0x86, 0xa6, 0x10, 0x36, 0x05, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x05, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00,
                0x00, 0x00, 0x61, 0x67, 0x65, 0x6e, 0x74, 0x50, 0x4b, 0x05, 0x06, 0x00, 0x00, 0x00,
                0x00, 0x01, 0x00, 0x01, 0x00, 0x33, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00,
                0x00,
            ];
            let expected_sha_256 = format!("{:X}", Sha256::digest(&archive));
            let client = StaticResponseClient { body: archive };
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");

            download_server_binary(
                &client,
                "https://example.com/agent.zip",
                Some(&expected_sha_256),
                &destination_path,
                AssetKind::Zip,
            )
            .await
            .unwrap();

            assert_eq!(
                std::fs::read(destination_path.join("agent")).unwrap(),
                b"hello"
            );
        });
    }

    #[test]
    fn extracts_tar_gz_with_pax_value_containing_newlines() {
        futures::executor::block_on(async {
            let archive = tar_gz_with_pax_value_containing_newlines();
            let client = StaticResponseClient { body: archive };
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");

            download_server_binary(
                &client,
                "https://example.com/agent.tar.gz",
                None,
                &destination_path,
                AssetKind::TarGz,
            )
            .await
            .unwrap();

            assert_eq!(
                std::fs::read(destination_path.join("agent")).unwrap(),
                b"hello\n"
            );
        });
    }

    #[test]
    fn extracts_tar_gz_when_pax_header_read_is_interrupted() {
        futures::executor::block_on(async {
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");
            let archive = PendingChunkedReader::new(tar_gz_with_pax_value_containing_newlines());

            extract_tar_gz(
                &destination_path,
                "https://example.com/agent.tar.gz",
                archive,
            )
            .await
            .unwrap();

            assert_eq!(
                std::fs::read(destination_path.join("agent")).unwrap(),
                b"hello\n"
            );
        });
    }

    #[test]
    fn extracts_large_pax_header_when_async_read_is_interrupted() {
        futures::executor::block_on(async {
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");
            let archive = PendingChunkedReader::new(tar_with_large_pax_value());

            unpack_tar_archive(
                &destination_path,
                "https://example.com/agent.tar.gz",
                archive,
            )
            .await
            .unwrap();

            assert_eq!(
                std::fs::read(destination_path.join("agent")).unwrap(),
                b"hello\n"
            );
        });
    }

    #[test]
    fn archive_digest_mismatch_prevents_extraction_and_cleans_up_staging() {
        futures::executor::block_on(async {
            let temp_dir = tempfile::tempdir().unwrap();
            let destination_path = temp_dir.path().join("v_1");
            let client = StaticResponseClient {
                body: b"not an archive".to_vec(),
            };

            let error = download_server_binary(
                &client,
                "https://example.com/agent.zip",
                Some("0000000000000000000000000000000000000000000000000000000000000000"),
                &destination_path,
                AssetKind::Zip,
            )
            .await
            .unwrap_err();

            assert!(error.to_string().contains("SHA-256 mismatch"));
            assert!(!destination_path.exists());
            let leftover_entries = std::fs::read_dir(temp_dir.path()).unwrap().count();
            assert_eq!(leftover_entries, 0, "staging directory should be removed");
        });
    }
}
