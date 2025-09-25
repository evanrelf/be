use crate::{
    context::cx,
    exec::exec,
    utils::flatten,
};
use camino::Utf8PathBuf;
use color_eyre::eyre;
use std::str::from_utf8;

#[tracing::instrument]
pub async fn changed_haskell_files() -> eyre::Result<Vec<Utf8PathBuf>> {
    // Chosen by `fd -e hs | cut -d '/' -f 1 | sort | uniq --count`
    let mut paths =
        changed_files(&["src/", "test/", "local-packages/", "nix/packages/mercury/"]).await?;
    paths.retain(|path| path.extension() == Some("hs"));
    Ok(paths)
}

#[tracing::instrument]
pub async fn changed_nix_files() -> eyre::Result<Vec<Utf8PathBuf>> {
    let mut paths = changed_files(&["."]).await?;
    paths.retain(|path| path.extension() == Some("nix"));
    Ok(paths)
}

#[tracing::instrument]
pub async fn changed_files(paths: &[&'static str]) -> eyre::Result<Vec<Utf8PathBuf>> {
    let cx = cx();

    let git = cx.cache.which("git").await?;

    let git_root = cx.cache.git_root().await?;

    let tracked_files_handle = {
        let git = git.clone();
        let mut args = vec![
            "-C",
            git_root.as_str(),
            "diff",
            "--diff-filter=dt",
            "--name-only",
            "--merge-base",
            "origin/master",
            "--",
        ];
        args.extend(paths);
        tokio::spawn(async move { exec(git, args).await })
    };

    let untracked_files_handle = {
        let git = git.clone();
        let mut args = vec![
            "-C",
            git_root.as_str(),
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
        ];
        args.extend(paths);
        tokio::spawn(async move { exec(git, args).await })
    };

    let (tracked_files_bytes, untracked_files_bytes) = tokio::try_join!(
        flatten(tracked_files_handle),
        flatten(untracked_files_handle)
    )?;

    let tracked_files = from_utf8(&tracked_files_bytes)?
        .lines()
        .map(Utf8PathBuf::from);

    let untracked_files = from_utf8(&untracked_files_bytes)?
        .lines()
        .map(Utf8PathBuf::from);

    let files = tracked_files.chain(untracked_files).collect();

    Ok(files)
}
