use crate::{
    exec::{self, exec, sandbox_exec},
    hashing::WithHashingExt as _,
    io::read_file,
    lint::HlintHint,
};
use bytes::BytesMut;
use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{self, ContextCompat as _};
use const_random::const_random;
use dashmap::DashMap;
use etcetera::app_strategy::{AppStrategy as _, AppStrategyArgs, Xdg};
use saphyr::{LoadableYamlNode as _, Yaml};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqliteSynchronous};
use std::{
    hash::Hasher as _,
    str::{self, FromStr as _},
};
use tempfile::tempdir;
use tokio::{
    fs::{self, File},
    io::AsyncReadExt as _,
    sync::OnceCell,
};
use twox_hash::XxHash3_64;
use which::{which_global, which_in_global};

// TODO: Only re-generated when this file is rebuilt
const BE_BINARY_ID: u64 = const_random!(u64);

pub struct Cache {
    sqlite: SqlitePool,
    git_root: OnceCell<Utf8PathBuf>,
    which: DashMap<&'static str, Utf8PathBuf>,
    fourmolu_version: OnceCell<String>,
    fourmolu_config: OnceCell<(Utf8PathBuf, u64)>,
    fourmolu_extensions: OnceCell<(Vec<String>, u64)>,
    nixfmt_version: OnceCell<String>,
    hlint_version: OnceCell<String>,
    hlint_configs: OnceCell<(Vec<Utf8PathBuf>, u64)>,
}

impl Cache {
    #[tracing::instrument]
    pub async fn new() -> eyre::Result<Self> {
        let xdg = Xdg::new(AppStrategyArgs {
            top_level_domain: String::from("com"),
            author: String::from("Evan Relf"),
            app_name: String::from("Be"),
        })?;

        let xdg_cache_dir = xdg.cache_dir();

        fs::create_dir_all(&xdg_cache_dir).await?;

        let sqlite_path = xdg.in_cache_dir("cache.sqlite");
        let sqlite_path = sqlite_path.to_str().unwrap();

        let sqlite_url = format!("sqlite://{sqlite_path}");

        let sqlite_opts = SqliteConnectOptions::from_str(&sqlite_url)?
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            // .pragma("mmap_size", u32::MAX.to_string())
            .create_if_missing(true);

        let sqlite = SqlitePool::connect_with(sqlite_opts).await?;

        if sqlite_valid(&sqlite).await? {
            tracing::debug!("Using existing SQLite cache (exists and has same `be` binary ID)");
        } else {
            tracing::debug!("Creating new SQLite cache (missing or different `be` binary ID)");
            sqlite_reset(&sqlite).await?;
        }

        Ok(Self {
            sqlite,
            git_root: OnceCell::new(),
            which: DashMap::new(),
            fourmolu_version: OnceCell::new(),
            fourmolu_config: OnceCell::new(),
            fourmolu_extensions: OnceCell::new(),
            nixfmt_version: OnceCell::new(),
            hlint_version: OnceCell::new(),
            hlint_configs: OnceCell::new(),
        })
    }

    #[tracing::instrument(skip_all)]
    pub async fn git_root(&self) -> eyre::Result<&Utf8PathBuf> {
        self.git_root
            .get_or_try_init(|| async {
                let git = Utf8PathBuf::try_from(which_global("git")?)?;
                let git_root = git_root(&git).await?;
                Ok(git_root)
            })
            .await
    }

    #[tracing::instrument(skip(self))]
    pub async fn which(&self, binary: &'static str) -> eyre::Result<Utf8PathBuf> {
        let git_root = self.git_root().await?;
        let bin = git_root.join(".bin");
        let which_path = || eyre::Ok(which_global(binary)?);
        let which_bin = || {
            let mut iter = which_in_global(binary, Some(bin))?;
            let path = iter.next().ok_or(which::Error::CannotFindBinaryPath)?;
            eyre::Ok(path)
        };
        let path = self.which.entry(binary).or_try_insert_with(|| {
            let path = which_path().or_else(|_| which_bin())?.canonicalize()?;
            let utf8_path = Utf8PathBuf::try_from(path)?;
            eyre::Ok(utf8_path)
        })?;
        Ok(path.clone())
    }

    #[tracing::instrument(skip(self))]
    pub async fn fourmolu_version(&self) -> eyre::Result<&str> {
        self.fourmolu_version
            .get_or_try_init(|| async {
                let fourmolu = self.which("fourmolu").await?;
                let stdout = sandbox_exec(exec::FOURMOLU_PROFILE, fourmolu, ["--version"]).await?;
                let version = String::from(str::from_utf8(&stdout)?.trim_end());
                Ok(version)
            })
            .await
            .map(|x| x.as_ref())
    }

    #[tracing::instrument(skip(self))]
    pub async fn fourmolu_config(&self) -> eyre::Result<&(Utf8PathBuf, u64)> {
        self.fourmolu_config
            .get_or_try_init(|| async {
                let git_root = self.git_root().await?;
                let path = git_root.join("fourmolu.yaml");
                let temp_dir = tempdir()?;
                let temp_path = Utf8PathBuf::try_from(temp_dir.path().join("fourmolu.yaml"))?;
                let copy_handle = tokio::spawn(fs::copy(path.clone(), temp_path.clone()));
                let hash_handle = tokio::spawn(async move { file_hash(&path).await });
                copy_handle.await??;
                let hash = hash_handle.await??;
                // TODO: gross
                std::mem::forget(temp_dir);
                Ok((temp_path, hash))
            })
            .await
    }

    #[tracing::instrument(skip(self))]
    pub async fn fourmolu_extensions(&self) -> eyre::Result<&(Vec<String>, u64)> {
        self.fourmolu_extensions
            .get_or_try_init(|| async {
                let git_root = self.git_root().await?;
                let path = git_root.join("hpack-common/default-extensions.yaml");
                let (bytes, _) = read_file(&path).await?;
                let str = str::from_utf8(&bytes)?;
                let yaml = Yaml::load_from_str(str)?;
                let extension_yamls = yaml
                    .first()
                    .context("Missing first YAML document")?
                    .as_mapping_get("default-extensions")
                    .context("Missing `default-extensions` key")?
                    .as_sequence()
                    .context("`default-extensions` is not a sequence")?;
                let mut extensions = Vec::with_capacity(extension_yamls.len());
                let mut hasher = XxHash3_64::default();
                for extension_yaml in extension_yamls {
                    let extension_str = extension_yaml
                        .as_str()
                        .context("Extension YAML is not a string")?;
                    hasher.write(extension_str.as_bytes());
                    extensions.push(String::from(extension_str));
                }
                let hash = hasher.finish();
                Ok((extensions, hash))
            })
            .await
    }

    #[tracing::instrument(skip(self))]
    pub async fn nixfmt_version(&self) -> eyre::Result<&str> {
        self.nixfmt_version
            .get_or_try_init(|| async {
                let fourmolu = self.which("nixfmt").await?;
                let stdout = sandbox_exec(exec::NIXFMT_PROFILE, fourmolu, ["--version"]).await?;
                let version = String::from(str::from_utf8(&stdout)?.trim_end());
                Ok(version)
            })
            .await
            .map(|x| x.as_ref())
    }

    #[tracing::instrument(skip_all)]
    pub async fn is_haskell_formatted(&self, source_hash: u64) -> eyre::Result<bool> {
        let version = self.fourmolu_version().await?;

        let (_, config_hash) = self.fourmolu_config().await?;

        let (_, extensions_hash) = self.fourmolu_extensions().await?;

        let is_formatted = sqlx::query_scalar(
            "
            select exists(
                select *
                from fourmolu
                where version = $1
                  and config_hash = $2
                  and extensions_hash = $3
                  and source_hash = $4
            )
            ",
        )
        .bind(version)
        .bind(config_hash.to_string())
        .bind(extensions_hash.to_string())
        .bind(source_hash.to_string())
        .fetch_one(&self.sqlite)
        .await?;

        Ok(is_formatted)
    }

    #[tracing::instrument(skip_all)]
    pub async fn mark_haskell_formatted(&self, source_hash: u64) -> eyre::Result<()> {
        let version = self.fourmolu_version().await?;

        let (_, config_hash) = self.fourmolu_config().await?;

        let (_, extensions_hash) = self.fourmolu_extensions().await?;

        sqlx::query("insert or ignore into fourmolu values ($1, $2, $3, $4)")
            .bind(version)
            .bind(config_hash.to_string())
            .bind(extensions_hash.to_string())
            .bind(source_hash.to_string())
            .execute(&self.sqlite)
            .await?;

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn is_nix_formatted(&self, source_hash: u64) -> eyre::Result<bool> {
        let version = self.nixfmt_version().await?;

        let is_formatted = sqlx::query_scalar(
            "
            select exists(
                select *
                from nixfmt
                where version = $1
                  and source_hash = $3
            )
            ",
        )
        .bind(version)
        .bind(source_hash.to_string())
        .fetch_one(&self.sqlite)
        .await?;

        Ok(is_formatted)
    }

    #[tracing::instrument(skip_all)]
    pub async fn mark_nix_formatted(&self, source_hash: u64) -> eyre::Result<()> {
        let version = self.nixfmt_version().await?;

        sqlx::query("insert or ignore into nixfmt values ($1, $2)")
            .bind(version)
            .bind(source_hash.to_string())
            .execute(&self.sqlite)
            .await?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn hlint_version(&self) -> eyre::Result<&str> {
        self.hlint_version
            .get_or_try_init(|| async {
                let fourmolu = self.which("hlint").await?;
                let stdout = sandbox_exec(exec::HLINT_PROFILE, fourmolu, ["--version"]).await?;
                let version = String::from(str::from_utf8(&stdout)?.trim_end());
                Ok(version)
            })
            .await
            .map(|x| x.as_ref())
    }

    // TODO: Refactor this, it's too long and verbose
    #[tracing::instrument(skip(self))]
    pub async fn hlint_configs(&self) -> eyre::Result<&(Vec<Utf8PathBuf>, u64)> {
        self.hlint_configs
            .get_or_try_init(|| async {
                let git_root = self.git_root().await?;
                let temp_dir = tempdir()?;
                let mut paths = Vec::new();
                let mut copy_handles = Vec::new();
                let mut hasher = XxHash3_64::default();

                let hlint_yaml = git_root.join(".hlint.yaml");
                if fs::metadata(&hlint_yaml).await.is_ok() {
                    let hash = file_hash(&hlint_yaml).await?;
                    hasher.write(&hash.to_le_bytes());
                    let file_name = hlint_yaml.file_name().unwrap();
                    let temp_path = Utf8PathBuf::try_from(temp_dir.path().join(file_name))?;
                    let copy_handle = tokio::spawn(fs::copy(hlint_yaml, temp_path.clone()));
                    copy_handles.push(copy_handle);
                    paths.push(temp_path);
                }

                let hlint_rules_dir = git_root.join("hlint-rules");
                if let Ok(mut dir) = fs::read_dir(&hlint_rules_dir).await {
                    while let Ok(Some(entry)) = dir.next_entry().await {
                        let Ok(file_type) = entry.file_type().await else {
                            continue;
                        };
                        if !file_type.is_file() {
                            continue;
                        }
                        let path = entry.path();
                        let Some(extension) = path.extension() else {
                            continue;
                        };
                        if extension != "yaml" {
                            continue;
                        }
                        let path = Utf8PathBuf::try_from(path)?;
                        let hash = file_hash(&path).await?;
                        hasher.write(&hash.to_le_bytes());
                        let file_name = path.file_name().unwrap();
                        let temp_path = Utf8PathBuf::try_from(temp_dir.path().join(file_name))?;
                        let copy_handle = tokio::spawn(fs::copy(path, temp_path.clone()));
                        copy_handles.push(copy_handle);
                        paths.push(temp_path);
                    }
                }

                for copy_handle in copy_handles {
                    copy_handle.await??;
                }

                let hash = hasher.finish();

                // TODO: gross
                std::mem::forget(temp_dir);

                Ok((paths, hash))
            })
            .await
    }

    #[tracing::instrument(skip_all)]
    pub async fn is_haskell_linted(
        &self,
        source_hash: u64,
    ) -> eyre::Result<Option<Vec<HlintHint>>> {
        let version = self.hlint_version().await?;

        let (_, configs_hash) = self.hlint_configs().await?;

        let hints_bytes: Option<Vec<u8>> = sqlx::query_scalar(
            "
            select hints
            from hlint
            where version = $1
              and configs_hash = $2
              and source_hash = $3
            ",
        )
        .bind(version)
        .bind(configs_hash.to_string())
        .bind(source_hash.to_string())
        .fetch_optional(&self.sqlite)
        .await?;

        if let Some(hints_bytes) = hints_bytes {
            let hints = serde_json::from_slice(&hints_bytes)?;
            Ok(Some(hints))
        } else {
            Ok(None)
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn mark_haskell_linted(
        &self,
        source_hash: u64,
        hints: &[HlintHint],
    ) -> eyre::Result<()> {
        let version = self.hlint_version().await?;

        let (_, configs_hash) = self.hlint_configs().await?;

        let hints = serde_json::to_vec(hints)?;

        sqlx::query("insert or ignore into hlint values ($1, $2, $3, $4)")
            .bind(version)
            .bind(configs_hash.to_string())
            .bind(source_hash.to_string())
            .bind(hints)
            .execute(&self.sqlite)
            .await?;

        Ok(())
    }
}

#[tracing::instrument(skip_all)]
async fn sqlite_valid(sqlite: &SqlitePool) -> eyre::Result<bool> {
    sqlx::raw_sql(
        "
        create table if not exists be_binary_id (
            be_binary_id text primary key
        ) strict
        ",
    )
    .execute(sqlite)
    .await?;

    let id_count: i64 = sqlx::query_scalar("select count(*) from be_binary_id")
        .fetch_one(sqlite)
        .await?;

    let has_id: bool = sqlx::query_scalar(
        "
        select exists(
            select be_binary_id from be_binary_id where be_binary_id = $1
        )
        ",
    )
    .bind(BE_BINARY_ID.to_string())
    .fetch_one(sqlite)
    .await?;

    Ok(id_count == 1 && has_id)
}

#[tracing::instrument(skip_all)]
async fn sqlite_reset(sqlite: &SqlitePool) -> eyre::Result<()> {
    sqlx::raw_sql(
        "
        drop table if exists be_binary_id;

        drop table if exists fourmolu;

        drop table if exists nixfmt;

        drop table if exists hlint;

        create table be_binary_id (
            be_binary_id text primary key not null
        ) strict;

        create table fourmolu (
            version text not null,
            config_hash text not null,
            extensions_hash text not null,
            source_hash text not null,
            unique (version, config_hash, source_hash)
        ) strict;

        create table nixfmt (
            version text not null,
            source_hash text not null,
            unique (version, source_hash)
        ) strict;

        create table hlint (
            version text not null,
            configs_hash text not null,
            source_hash text not null,
            hints blob not null,
            unique (version, configs_hash, source_hash)
        ) strict;
        ",
    )
    .execute(sqlite)
    .await?;

    sqlx::query("insert into be_binary_id values ($1)")
        .bind(BE_BINARY_ID.to_string())
        .execute(sqlite)
        .await?;

    Ok(())
}

// TODO: This might be incorrect? Hashing the `be` binary wasn't working.
#[tracing::instrument]
async fn file_hash(path: &Utf8Path) -> eyre::Result<u64> {
    let mut buffer = BytesMut::with_capacity(1024);
    let mut file = File::open(path).await?.with_hashing();
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        buffer.clear();
    }
    Ok(file.hash())
}

#[tracing::instrument(skip_all)]
async fn git_root(git: &Utf8Path) -> eyre::Result<Utf8PathBuf> {
    let stdout = exec(git, ["rev-parse", "--show-toplevel"]).await?;
    let root = Utf8PathBuf::from(str::from_utf8(&stdout)?.trim_end());
    Ok(root)
}
