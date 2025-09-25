use bytes::Bytes;
use color_eyre::eyre;
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::ExitStatusExt as _,
};
use tokio::process::Command;

#[tracing::instrument(
    skip_all,
    fields(%program = program.as_ref().to_string_lossy()),
)]
pub async fn exec(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> eyre::Result<Bytes> {
    tracing::trace!("Spawning");

    let output = Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await?;

    tracing::trace!("Finished");

    if !output.status.success() {
        if let Some(exit_code) = output.status.code() {
            eyre::bail!(
                "Child process exited with code {exit_code}:\n{}",
                String::from_utf8_lossy(&output.stderr),
            );
        } else if let Some(signal) = output.status.signal() {
            eyre::bail!("Child process was terminated by signal {signal}");
        } else {
            eyre::bail!("Child process died of unknown causes");
        }
    }

    Ok(Bytes::from(output.stdout))
}

pub async fn sandbox_exec(
    profile: &str,
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> eyre::Result<Bytes> {
    if cfg!(target_os = "macos") {
        let program_args = args.into_iter().map(|arg| arg.as_ref().to_os_string());
        let mut args = vec![
            OsString::from("-p"),
            OsString::from(profile),
            OsString::from("--"),
            program.as_ref().to_os_string(),
        ];
        args.extend(program_args);
        exec("/usr/bin/sandbox-exec", args).await
    } else {
        exec(program, args).await
    }
}

pub const FOURMOLU_PROFILE: &str = r#"
(version 1)
(deny default)
(allow process-exec*
  (regex #"^/nix/store/[a-z0-9]+-fourmolu-[^/]+/bin/fourmolu$"))
(allow file-read*)
(deny file-read*
  (subpath "/Users"))
"#;

pub const NIXFMT_PROFILE: &str = r#"
(version 1)
(deny default)
(allow process-exec*
  (regex #"^/nix/store/[a-z0-9]+-nixfmt-[^/]+/bin/nixfmt$"))
(allow file-read*)
(deny file-read*
  (subpath "/Users"))
"#;

// TODO: Lock this down further
pub const HLINT_PROFILE: &str = r#"
(version 1)
(allow default)
(deny file-read*
  (subpath "/Users"))
"#;
