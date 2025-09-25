use crate::{
    cli::format::{Args, Command, HaskellArgs, NixArgs},
    context::cx,
    exec, git,
    io::{read_file, read_stdin, write_file, write_stdout},
    utils::flatten,
};
use bytes::Bytes;
use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre;
use num_format::{Locale, ToFormattedString as _};
use std::{os::unix::process::ExitStatusExt as _, process::Stdio};
use tokio::{fs, io::AsyncWriteExt as _, process};
use tracing_indicatif::indicatif_eprintln;

#[tracing::instrument(skip_all)]
pub async fn run(args: &Args) -> eyre::Result<()> {
    if let Some(Command::Haskell(args)) = &args.command {
        run_format_haskell(args).await?;
        return Ok(());
    }

    if let Some(Command::Nix(args)) = &args.command {
        run_format_nix(args).await?;
        return Ok(());
    }

    let haskell = tokio::spawn(async {
        let args = HaskellArgs {
            paths: vec![],
            stdin: false,
        };
        run_format_haskell(&args).await
    });

    let nix = tokio::spawn(async {
        let args = NixArgs {
            paths: vec![],
            stdin: false,
        };
        run_format_nix(&args).await
    });

    tokio::try_join!(flatten(haskell), flatten(nix))?;

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn run_format_haskell(args: &HaskellArgs) -> eyre::Result<()> {
    let cx = cx();

    if args.stdin {
        let (input_bytes, input_hash) = read_stdin().await?;

        let output_bytes = if cx.cache.is_haskell_formatted(input_hash).await? {
            tracing::trace!("Skipping format");
            input_bytes
        } else {
            tracing::trace!("Formatting");
            fourmolu(None, input_bytes).await?
        };

        write_stdout(output_bytes).await?;

        return Ok(());
    }

    let changed_files = git::changed_haskell_files().await?;

    let paths = if args.paths.is_empty() {
        changed_files
    } else {
        args.paths.clone()
    };

    let mut handles = Vec::new();

    for path in paths {
        handles.push(tokio::spawn(async move { format_haskell(&path).await }));
    }

    let total_count = handles.len();
    let mut formatted_count = 0;

    for handle in handles {
        if let Some(true) = handle.await?? {
            formatted_count += 1;
        }
    }

    indicatif_eprintln!(
        "Formatted {formatted_count} of {total_count} Haskell {files}",
        formatted_count = formatted_count.to_formatted_string(&Locale::en),
        total_count = total_count.to_formatted_string(&Locale::en),
        files = if total_count == 1 { "file" } else { "files" },
    );

    Ok(())
}

#[tracing::instrument(fields(indicatif.pb_show))]
async fn format_haskell(path: &Utf8Path) -> eyre::Result<Option<bool>> {
    let cx = cx();

    let (input_bytes, input_hash) = read_file(path).await?;

    if cx.cache.is_haskell_formatted(input_hash).await? {
        tracing::trace!("Skipping format");
        return Ok(Some(false));
    }

    tracing::trace!("Formatting");

    let output_bytes = fourmolu(Some(path), input_bytes.clone()).await?;

    cx.cache.mark_haskell_formatted(input_hash).await?;

    if input_bytes == output_bytes {
        tracing::trace!("Skipping write");
        return Ok(Some(false));
    }

    tracing::trace!("Writing");

    write_file(path, output_bytes).await?;

    Ok(Some(true))
}

#[tracing::instrument(skip(bytes))]
async fn fourmolu(path: Option<&Utf8Path>, bytes: Bytes) -> eyre::Result<Bytes> {
    let cx = cx();

    let fourmolu = &cx.cache.which("fourmolu").await?;

    let path = match path {
        Some(path) => Utf8PathBuf::try_from(fs::canonicalize(path).await?).unwrap(),
        None => Utf8PathBuf::from("<stdin>"),
    };

    let (config, _) = cx.cache.fourmolu_config().await?;

    let (extensions, _) = cx.cache.fourmolu_extensions().await?;

    let mut args = Vec::new();

    args.push(format!("--config={config}"));
    args.push(String::from("--no-cabal"));
    args.push(format!("--stdin-input-file={path}"));
    args.push(String::from("--mode=stdout"));
    args.push(String::from("--source-type=module"));
    args.push(String::from("--unsafe"));
    args.push(String::from("--quiet"));

    for extension in extensions {
        args.push(format!("--ghc-opt=-X{extension}"));
    }

    let file_permit = cx.file_permits.acquire().await?;
    let process_permit = cx.process_permits.acquire().await?;

    let mut command = if cfg!(target_os = "macos") {
        let mut command = process::Command::new("/usr/bin/sandbox-exec");
        command.arg("-p");
        command.arg(exec::FOURMOLU_PROFILE);
        command.arg("--");
        command.arg(fourmolu);
        command
    } else {
        process::Command::new(fourmolu)
    };

    let mut child = command
        .args(args)
        .env_clear()
        .current_dir("/var/empty")
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();

    stdin.write_all(&bytes).await?;

    stdin.flush().await?;

    drop(stdin);

    let output = child.wait_with_output().await?;

    drop(process_permit);
    drop(file_permit);

    if !output.status.success() {
        if let Some(exit_code) = output.status.code() {
            eyre::bail!(
                "`fourmolu` exited with code {exit_code}:\n{}",
                String::from_utf8_lossy(&output.stderr),
            );
        } else if let Some(signal) = output.status.signal() {
            eyre::bail!("`fourmolu` was terminated by signal {signal}");
        } else {
            eyre::bail!("`fourmolu` died of unknown causes");
        }
    }

    Ok(Bytes::from(output.stdout))
}

#[tracing::instrument(skip_all)]
pub async fn run_format_nix(args: &NixArgs) -> eyre::Result<()> {
    let cx = cx();

    if args.stdin {
        let (input_bytes, input_hash) = read_stdin().await?;

        let output_bytes = if cx.cache.is_nix_formatted(input_hash).await? {
            tracing::trace!("Skipping format");
            input_bytes
        } else {
            tracing::trace!("Formatting");
            nixfmt(None, input_bytes).await?
        };

        write_stdout(output_bytes).await?;

        return Ok(());
    }

    let changed_files = git::changed_nix_files().await?;

    let paths = if args.paths.is_empty() {
        changed_files
    } else {
        args.paths.clone()
    };

    let mut handles = Vec::new();

    for path in paths {
        handles.push(tokio::spawn(async move { format_nix(&path).await }));
    }

    let total_count = handles.len();
    let mut formatted_count = 0;

    for handle in handles {
        if let Some(true) = handle.await?? {
            formatted_count += 1;
        }
    }

    indicatif_eprintln!(
        "Formatted {formatted_count} of {total_count} Nix {files}",
        formatted_count = formatted_count.to_formatted_string(&Locale::en),
        total_count = total_count.to_formatted_string(&Locale::en),
        files = if total_count == 1 { "file" } else { "files" },
    );

    Ok(())
}

#[tracing::instrument(fields(indicatif.pb_show))]
async fn format_nix(path: &Utf8Path) -> eyre::Result<Option<bool>> {
    let cx = cx();

    let (input_bytes, input_hash) = read_file(path).await?;

    if cx.cache.is_nix_formatted(input_hash).await? {
        tracing::trace!("Skipping format");
        return Ok(Some(false));
    }

    tracing::trace!("Formatting");

    let output_bytes = nixfmt(Some(path), input_bytes.clone()).await?;

    cx.cache.mark_nix_formatted(input_hash).await?;

    if input_bytes == output_bytes {
        tracing::trace!("Skipping write");
        return Ok(Some(false));
    }

    tracing::trace!("Writing");

    write_file(path, output_bytes).await?;

    Ok(Some(true))
}

#[tracing::instrument(skip(bytes))]
async fn nixfmt(path: Option<&Utf8Path>, bytes: Bytes) -> eyre::Result<Bytes> {
    let cx = cx();

    let nixfmt = &cx.cache.which("nixfmt").await?;

    let path = match path {
        Some(path) => Utf8PathBuf::try_from(fs::canonicalize(path).await?).unwrap(),
        None => Utf8PathBuf::from("<stdin>"),
    };

    let file_permit = cx.file_permits.acquire().await?;
    let process_permit = cx.process_permits.acquire().await?;

    let mut command = if cfg!(target_os = "macos") {
        let mut command = process::Command::new("/usr/bin/sandbox-exec");
        command.arg("-p");
        command.arg(exec::NIXFMT_PROFILE);
        command.arg("--");
        command.arg(nixfmt);
        command
    } else {
        process::Command::new(nixfmt)
    };

    let mut child = command
        .args([&format!("--filename={path}"), "-"])
        .env_clear()
        .current_dir("/var/empty")
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();

    stdin.write_all(&bytes).await?;

    stdin.flush().await?;

    drop(stdin);

    let output = child.wait_with_output().await?;

    drop(process_permit);
    drop(file_permit);

    if !output.status.success() {
        if let Some(exit_code) = output.status.code() {
            eyre::bail!(
                "`nixfmt` exited with code {exit_code}:\n{}",
                String::from_utf8_lossy(&output.stderr),
            );
        } else if let Some(signal) = output.status.signal() {
            eyre::bail!("`nixfmt` was terminated by signal {signal}");
        } else {
            eyre::bail!("`nixfmt` died of unknown causes");
        }
    }

    Ok(Bytes::from(output.stdout))
}
