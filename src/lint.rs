use crate::{
    cli::lint::{Args, Command, HaskellArgs},
    context::cx,
    exec, git,
    io::read_file,
};
use bytes::Bytes;
use camino::Utf8Path;
use color_eyre::eyre;
use derive_more::Display;
use num_format::{Locale, ToFormattedString as _};
use std::{
    fmt::{self, Display},
    io::IsTerminal as _,
    os::unix::process::ExitStatusExt as _,
    process::Stdio,
};
use tokio::{io::AsyncWriteExt as _, process};
use tracing_indicatif::{indicatif_eprintln, indicatif_println};

#[tracing::instrument(skip_all)]
pub async fn run(args: &Args) -> eyre::Result<()> {
    if let Some(Command::Haskell(args)) = &args.command {
        run_lint_haskell(args).await?;
        return Ok(());
    }

    let haskell = tokio::spawn(async {
        let args = HaskellArgs {
            paths: vec![],
            stdin: false,
        };
        run_lint_haskell(&args).await
    });

    haskell.await??;

    Ok(())
}

// TODO: Handle input on `stdin`
#[tracing::instrument(skip_all)]
async fn run_lint_haskell(args: &HaskellArgs) -> eyre::Result<()> {
    let changed_files = git::changed_haskell_files().await?;

    let paths = if args.paths.is_empty() {
        changed_files
    } else {
        args.paths.clone()
    };

    let mut handles = Vec::new();

    for path in paths {
        handles.push(tokio::spawn(async move { lint_haskell(&path).await }));
    }

    let total_count = handles.len();
    let mut linted_count = 0;

    for handle in handles {
        if let Some(true) = handle.await?? {
            linted_count += 1;
        }
    }

    indicatif_eprintln!(
        "Linted {linted_count} of {total_count} Haskell {files}",
        linted_count = linted_count.to_formatted_string(&Locale::en),
        total_count = total_count.to_formatted_string(&Locale::en),
        files = if total_count == 1 { "file" } else { "files" },
    );

    Ok(())
}

#[tracing::instrument(fields(indicatif.pb_show))]
async fn lint_haskell(path: &Utf8Path) -> eyre::Result<Option<bool>> {
    let cx = cx();

    let (input_bytes, input_hash) = read_file(path).await?;

    if let Some(hints) = cx.cache.is_haskell_linted(input_hash).await? {
        tracing::trace!("Using cached lint results");
        for hint in hints {
            indicatif_println!("{hint}");
        }
        return Ok(Some(false));
    }

    tracing::trace!("Linting");

    let hints = hlint(Some(path), input_bytes).await?;

    for hint in &hints {
        indicatif_println!("{hint}");
    }

    cx.cache.mark_haskell_linted(input_hash, &hints).await?;

    Ok(Some(true))
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HlintHint {
    module: Vec<String>,
    decl: Vec<String>,
    severity: HlintSeverity,
    hint: String,
    file: String,
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
    from: String,
    to: Option<String>,
    note: Vec<String>,
    refactorings: String,
}

impl Display for HlintHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        let Self {
            file,
            start_line,
            start_column,
            severity,
            hint,
            from,
            ..
        } = self;
        // TODO: Go beyond MVP formatting
        let first_line = format!("{file}:{start_line}:{start_column}: {severity}: {hint}");
        if std::io::stdout().is_terminal() {
            // Bold and underline
            writeln!(f, "\x1b[1m\x1b[4m{first_line}\x1b[0m")?;
        } else {
            writeln!(f, "{first_line}")?;
        }
        writeln!(f, "Found:\n  {from}")?;
        if let Some(to) = &self.to {
            writeln!(f, "Perhaps:\n  {to}")?;
        }
        for note in &self.note {
            writeln!(f, "Note: {note}")?;
        }
        Ok(())
    }
}

#[derive(Display, serde::Deserialize, serde::Serialize)]
enum HlintSeverity {
    Ignore,
    Suggestion,
    Warning,
    Error,
}

// TODO: Do an `strace`-style tracking of files it reads and processes it spawns. Might be reading
// Haskell files or talking to Git to infer language extensions and files to look at respectively.
#[tracing::instrument(skip(bytes))]
async fn hlint(path: Option<&Utf8Path>, bytes: Bytes) -> eyre::Result<Vec<HlintHint>> {
    let cx = cx();

    let hlint = &cx.cache.which("hlint").await?;

    let file_permit = cx.file_permits.acquire().await?;
    let process_permit = cx.process_permits.acquire().await?;

    let mut command = if cfg!(target_os = "macos") {
        let mut command = process::Command::new("/usr/bin/sandbox-exec");
        command.arg("-p");
        command.arg(exec::HLINT_PROFILE);
        command.arg("--");
        command.arg(hlint);
        command
    } else {
        process::Command::new(hlint)
    };

    let (hlint_configs, _) = cx.cache.hlint_configs().await?;

    let mut args = vec![
        String::from("--json"),
        String::from("--no-exit-code"),
        String::from("-"),
    ];

    for config in hlint_configs {
        args.push(format!("--hint={config}"));
    }

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
                "`hlint` exited with code {exit_code}:\n{}",
                String::from_utf8_lossy(&output.stderr),
            );
        } else if let Some(signal) = output.status.signal() {
            eyre::bail!("`hlint` was terminated by signal {signal}");
        } else {
            eyre::bail!("`hlint` died of unknown causes");
        }
    }

    let mut hints: Vec<HlintHint> = serde_json::from_slice(&output.stdout)?;

    if let Some(path) = path {
        for hint in &mut hints {
            hint.file.clear();
            hint.file.push_str(path.as_str());
        }
    }

    Ok(hints)
}
