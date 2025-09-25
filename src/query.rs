use crate::{
    cli::query::{Args, Command, QueryArgs},
    io::{read_file, read_stdin},
};
use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre;
use etcetera::app_strategy::{AppStrategy as _, AppStrategyArgs, Xdg};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqliteSynchronous};
use std::str::{self, FromStr as _};
use std::sync::LazyLock;
use tokio::fs;
use tracing_indicatif::indicatif_println;
use tree_sitter::{Language, Node, Parser, QueryCursor, StreamingIterator as _, Tree};

#[tracing::instrument(skip_all)]
pub async fn run(args: &Args) -> eyre::Result<()> {
    match &args.command {
        Command::Index => run_query_index().await,
        Command::Imports(args) => run_query_imports(args).await,
    }
}

async fn run_query_index() -> eyre::Result<()> {
    let xdg = Xdg::new(AppStrategyArgs {
        top_level_domain: String::from("com"),
        author: String::from("Evan Relf"),
        app_name: String::from("Be"),
    })?;

    let xdg_cache_dir = xdg.cache_dir();

    fs::create_dir_all(&xdg_cache_dir).await?;

    let sqlite_path = xdg.in_cache_dir("query.sqlite");
    let sqlite_path = sqlite_path.to_str().unwrap();

    let sqlite_url = format!("sqlite://{sqlite_path}");

    let sqlite_opts = SqliteConnectOptions::from_str(&sqlite_url)?
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        // .pragma("mmap_size", u32::MAX.to_string())
        .create_if_missing(true);

    let sqlite = SqlitePool::connect_with(sqlite_opts).await?;

    sqlite_reset(&sqlite).await?;

    Ok(())
}

async fn sqlite_reset(sqlite: &SqlitePool) -> eyre::Result<()> {
    sqlx::raw_sql(
        "
        drop table if exists module_vertices;

        drop table if exists module_edges;

        create table module_vertices (
            path text primary key,
            name text not null
        ) strict;

        create table module_edges (
            source text not null references module_vertices,
            target text not null references module_vertices,
            unique (source, target)
        ) strict;
        ",
    )
    .execute(sqlite)
    .await?;

    Ok(())
}

////////////////////////////////////////////////////////////////////////////////////////////////////

static LANGUAGE: LazyLock<Language> = LazyLock::new(|| tree_sitter_haskell::LANGUAGE.into());

#[tracing::instrument(skip_all)]
pub async fn run_query_imports(args: &QueryArgs) -> eyre::Result<()> {
    let process = |path: Option<&Utf8Path>, bytes: &[u8]| {
        let source_code = str::from_utf8(bytes)?;
        let mut parser = Parser::new();
        parser.set_language(&LANGUAGE)?;
        let tree = parser.parse(source_code, None).unwrap();
        let items = query_imports(source_code, &tree)?;
        for Item { line, column, text } in items {
            let path = match path {
                Some(path) => path.as_str(),
                None => "<stdin>",
            };
            indicatif_println!("{path}:{line}:{column}:{text}");
        }
        eyre::Ok(())
    };

    if args.stdin {
        let (input_bytes, _input_hash) = read_stdin().await?;
        process(None, &input_bytes)?;
        return Ok(());
    }

    let mut handles = Vec::new();

    for module in &args.modules {
        let path = Utf8PathBuf::from(module);
        handles.push(tokio::spawn(async move {
            // TODO: Detect if module name, convert to path
            let (input_bytes, _input_hash) = read_file(&path).await?;
            process(Some(&path), &input_bytes)?;
            eyre::Ok(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

struct Item<'a> {
    line: usize,
    column: usize,
    text: &'a str,
}

fn query_imports<'a>(source_code: &'a str, tree: &'a Tree) -> eyre::Result<Vec<Item<'a>>> {
    query(
        source_code,
        tree,
        "(haskell (imports (import module: (_) @import)))",
    )
}

fn query<'a>(source_code: &'a str, tree: &'a Tree, query: &str) -> eyre::Result<Vec<Item<'a>>> {
    let root_node = tree.root_node();
    let query = tree_sitter::Query::new(&LANGUAGE, query)?;
    let mut query_cursor = QueryCursor::new();
    let mut query_matches = query_cursor.matches(&query, root_node, source_code.as_bytes());
    let mut items = Vec::with_capacity(query_matches.size_hint().0);
    while let Some(query_match) = query_matches.next() {
        for match_capture in query_match.captures {
            let node = match_capture.node;
            let range = node.range();
            let line = range.start_point.row;
            let column = range.start_point.column;
            let text = node_text(source_code, &node).unwrap();
            items.push(Item { line, column, text });
        }
    }
    Ok(items)
}

fn node_text<'a>(source_code: &'a str, node: &Node) -> Option<&'a str> {
    source_code.get(node.byte_range())
}
