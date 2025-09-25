use crate::{context::cx, hashing::WithHashingExt as _};
use bytes::Bytes;
use camino::Utf8Path;
use color_eyre::eyre;
use std::io::Write as _;
use tempfile::tempdir;
use tokio::{
    fs::{self, File},
    io::{self, AsyncReadExt as _, AsyncWriteExt as _},
};
use tracing_indicatif::writer::get_indicatif_stdout_writer;

#[tracing::instrument]
pub async fn read_stdin() -> eyre::Result<(Bytes, u64)> {
    let mut stdin = io::stdin().with_hashing();
    let mut bytes = Vec::new();
    stdin.read_to_end(&mut bytes).await?;
    let hash = stdin.hash();
    Ok((Bytes::from(bytes), hash))
}

#[tracing::instrument(skip_all)]
pub async fn write_stdout(bytes: Bytes) -> eyre::Result<()> {
    if let Some(mut stdout) = get_indicatif_stdout_writer() {
        tokio::task::spawn_blocking(move || stdout.write_all(&bytes)).await??;
    } else {
        io::stdout().write_all(&bytes).await?;
    }
    Ok(())
}

#[tracing::instrument]
pub async fn read_file(path: &Utf8Path) -> eyre::Result<(Bytes, u64)> {
    let cx = cx();
    let _permit = cx.file_permits.acquire().await?;
    let mut file = File::open(path).await?.with_hashing();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let hash = file.hash();
    Ok((Bytes::from(bytes), hash))
}

#[tracing::instrument(skip(bytes))]
pub async fn write_file(path: &Utf8Path, bytes: Bytes) -> eyre::Result<()> {
    let cx = cx();
    let temp_dir = tempdir()?;
    let temp_path = temp_dir.path().join(path.file_name().unwrap_or("temp"));
    let permit = cx.file_permits.acquire().await?;
    let mut temp_file = File::create(&temp_path).await?;
    temp_file.write_all(&bytes).await?;
    temp_file.flush().await?;
    drop(temp_file);
    drop(permit);
    fs::rename(temp_path, path).await?;
    Ok(())
}
