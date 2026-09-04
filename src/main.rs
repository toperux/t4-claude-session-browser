use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use claude_session_browser::index::{Index, Sort};
use claude_session_browser::paths::ClaudeDir;
use claude_session_browser::{cli, gui, tui};

/// Browse, preview and delete Claude Code sessions.
#[derive(Parser)]
#[command(name = "csb", version, about)]
struct Args {
    /// Claude config directory (default: $CLAUDE_CONFIG_DIR or ~/.claude)
    #[arg(long, global = true, value_name = "DIR")]
    claude_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the desktop browser (default when no command is given)
    Gui,
    /// Open the terminal browser
    Tui,
    /// List sessions
    List {
        /// Filter by project slug or path fragment, or "all"
        #[arg(long, short)]
        project: Option<String>,
        #[arg(long, value_enum, default_value = "date")]
        sort: Sort,
        #[arg(long)]
        json: bool,
    },
    /// List projects
    Projects,
    /// Print a session transcript
    Show {
        /// Session id or unique prefix
        id: String,
        /// Dump the original JSONL instead
        #[arg(long)]
        raw: bool,
        /// Include subagent sidechains
        #[arg(long)]
        sidechains: bool,
    },
    /// Move sessions and their sidecar dirs to the recycle bin
    Delete {
        /// Session ids or unique prefixes
        ids: Vec<String>,
        /// Also select sessions idle longer than this (e.g. 30d, 2w, 12h)
        #[arg(long, value_name = "SPEC")]
        older_than: Option<String>,
        /// Restrict --older-than to one project
        #[arg(long, short)]
        project: Option<String>,
        /// Show what would be deleted and stop
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt
        #[arg(long, short)]
        yes: bool,
        /// Allow deleting sessions that look active
        #[arg(long)]
        force: bool,
    },
    /// Update csb to the latest release
    Update {
        /// Only report whether an update exists
        #[arg(long)]
        check: bool,
        /// Reinstall the latest release even if this binary looks current.
        /// Use this to finish an update that was left half-applied.
        #[arg(long)]
        force: bool,
    },
}

/// Index for a CLI command, reporting unreadable files on stderr so they stay
/// out of piped stdout.
fn index_for_cli(dir: &ClaudeDir) -> Result<Index> {
    let index = Index::build(dir)?;
    for warning in &index.warnings {
        eprintln!("warning: skipping {warning}");
    }
    Ok(index)
}

fn main() -> Result<()> {
    let args = Args::parse();

    // `update` must work on a machine with no ~/.claude at all, so the directory
    // is resolved per-command rather than up front.
    if let Some(Cmd::Update { check, force }) = args.cmd {
        return cli::update(check, force);
    }
    let dir = ClaudeDir::resolve(args.claude_dir.as_deref())?;

    match args.cmd {
        None | Some(Cmd::Gui) => gui::run(dir),
        Some(Cmd::Tui) => tui::run(dir),
        Some(Cmd::List {
            project,
            sort,
            json,
        }) => {
            let index = index_for_cli(&dir)?;
            cli::list(&index, project.as_deref(), sort, json)
        }
        Some(Cmd::Projects) => {
            let index = index_for_cli(&dir)?;
            cli::projects(&index)
        }
        Some(Cmd::Show {
            id,
            raw,
            sidechains,
        }) => {
            let index = index_for_cli(&dir)?;
            cli::show(&index, &id, raw, sidechains)
        }
        Some(Cmd::Update { .. }) => unreachable!("handled above"),
        Some(Cmd::Delete {
            ids,
            older_than,
            project,
            dry_run,
            yes,
            force,
        }) => {
            let index = index_for_cli(&dir)?;
            cli::delete(
                &dir,
                &index,
                &ids,
                older_than.as_deref(),
                project.as_deref(),
                dry_run,
                yes,
                force,
            )
        }
    }
}
