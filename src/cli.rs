use anyhow::{bail, Context, Result};
use chrono::{Duration, Local, Utc};
use serde_json::json;
use std::cmp::Reverse;
use std::io::{BufRead, Write};

use crate::del::{self, human_bytes};
use crate::index::{Index, SessionMeta};
use crate::paths::ClaudeDir;
use crate::transcript::{self, Event, LoadOpts};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Sort {
    Date,
    Size,
    Msgs,
}

// Table headers stay as args so they share the row format string's width specs.
#[allow(clippy::print_literal)]
pub fn list(
    dir: &ClaudeDir,
    index: &Index,
    project: Option<&str>,
    sort: Sort,
    as_json: bool,
) -> Result<()> {
    let _ = dir;
    let mut sessions: Vec<&SessionMeta> = index
        .sessions
        .iter()
        .filter(|s| matches_project(s, project))
        .collect();

    match sort {
        Sort::Date => sessions.sort_by_key(|s| Reverse(s.activity())),
        Sort::Size => sessions.sort_by_key(|s| Reverse(s.size_bytes)),
        Sort::Msgs => sessions.sort_by_key(|s| Reverse(s.user_msgs + s.assistant_msgs)),
    }

    if as_json {
        let rows: Vec<_> = sessions
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "title": s.title,
                    "project": s.project_slug,
                    "cwd": s.cwd,
                    "gitBranch": s.git_branch,
                    "path": s.path,
                    "sizeBytes": s.size_bytes,
                    "lastActivity": s.activity().to_rfc3339(),
                    "userMessages": s.user_msgs,
                    "assistantMessages": s.assistant_msgs,
                    "toolCalls": s.tool_calls,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if sessions.is_empty() {
        println!("no sessions found");
        return Ok(());
    }

    let total: u64 = sessions.iter().map(|s| s.size_bytes).sum();
    println!(
        "{:<8}  {:<16}  {:>6}  {:>9}  {}",
        "ID", "LAST ACTIVITY", "MSGS", "SIZE", "TITLE"
    );
    for s in &sessions {
        println!(
            "{:<8}  {:<16}  {:>6}  {:>9}  {}",
            s.short_id(),
            s.activity()
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            s.user_msgs + s.assistant_msgs,
            human_bytes(s.size_bytes),
            crate::index::truncate(&s.title, 70),
        );
    }
    println!("\n{} sessions, {}", sessions.len(), human_bytes(total));
    Ok(())
}

// Table headers stay as args so they share the row format string's width specs.
#[allow(clippy::print_literal)]
pub fn projects(index: &Index) -> Result<()> {
    let projects = index.projects();
    if projects.is_empty() {
        println!("no projects found");
        return Ok(());
    }
    println!(
        "{:<6}  {:>9}  {:<40}  {}",
        "SESS", "SIZE", "SLUG", "LOCATION"
    );
    for p in &projects {
        println!(
            "{:<6}  {:>9}  {:<40}  {}",
            p.sessions.len(),
            human_bytes(p.size_bytes()),
            crate::index::truncate(&p.slug, 40),
            p.label,
        );
    }
    Ok(())
}

pub fn show(index: &Index, needle: &str, raw: bool, sidechains: bool) -> Result<()> {
    let meta = index.find(needle)?;
    if raw {
        let body = std::fs::read_to_string(&meta.path)?;
        print!("{body}");
        return Ok(());
    }

    println!("# {}", meta.title);
    println!("  id       {}", meta.id);
    println!("  cwd      {}", meta.location());
    if let Some(b) = &meta.git_branch {
        println!("  branch   {b}");
    }
    println!(
        "  activity {}",
        meta.activity()
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
    );
    println!(
        "  volume   {} messages, {} tool calls, {}",
        meta.user_msgs + meta.assistant_msgs,
        meta.tool_calls,
        human_bytes(meta.size_bytes)
    );
    println!();

    let opts = LoadOpts {
        max_entries: usize::MAX,
        include_sidechains: sidechains,
        include_meta: false,
    };
    let t = transcript::load(&meta.path, &opts)?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for entry in &t.entries {
        let marker = if entry.sidechain { "|" } else { " " };
        match &entry.event {
            Event::User(text) => writeln!(out, "{marker}> {text}\n")?,
            Event::Assistant(text) => writeln!(out, "{marker}  {text}\n")?,
            Event::Thinking(text) => {
                writeln!(out, "{marker}  ~ {}\n", crate::index::truncate(text, 300))?
            }
            Event::ToolUse { name, headline, .. } => {
                writeln!(out, "{marker}  * {name}: {headline}")?
            }
            Event::ToolResult {
                is_error, preview, ..
            } => {
                let tag = if *is_error { "!" } else { "=" };
                writeln!(
                    out,
                    "{marker}  {tag} {}",
                    crate::index::truncate(preview, 160)
                )?
            }
            Event::Meta(label) => writeln!(out, "{marker}  . {label}")?,
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn delete(
    dir: &ClaudeDir,
    index: &Index,
    ids: &[String],
    older_than: Option<&str>,
    project: Option<&str>,
    dry_run: bool,
    yes: bool,
    force: bool,
) -> Result<()> {
    let mut targets: Vec<&SessionMeta> = Vec::new();

    for needle in ids {
        let meta = index.find(needle)?;
        if !targets.iter().any(|t| t.id == meta.id) {
            targets.push(meta);
        }
    }

    if let Some(spec) = older_than {
        let cutoff = Utc::now() - parse_duration(spec)?;
        for s in &index.sessions {
            if s.activity() < cutoff
                && matches_project(s, project)
                && !targets.iter().any(|t| t.id == s.id)
            {
                targets.push(s);
            }
        }
    } else if ids.is_empty() {
        bail!("give session ids, or --older-than to select by age");
    }

    if targets.is_empty() {
        println!("nothing matched");
        return Ok(());
    }

    let plans: Vec<_> = targets.iter().map(|m| del::plan(dir, m)).collect();
    let total: u64 = plans.iter().map(|p| p.bytes).sum();

    for p in &plans {
        let flag = if p.recent { "  [ACTIVE?]" } else { "" };
        println!("{} {}{flag}", p.short_id(), p.title);
        for path in &p.paths {
            println!("    {}", path.display());
        }
    }
    println!(
        "\n{} session(s), {} to the recycle bin",
        plans.len(),
        human_bytes(total)
    );

    if dry_run {
        println!("(dry run - nothing deleted)");
        return Ok(());
    }

    let live: Vec<&str> = plans
        .iter()
        .filter(|p| p.recent)
        .map(|p| p.short_id())
        .collect();
    if !live.is_empty() && !force {
        bail!(
            "{} session(s) were active in the last 5 minutes ({}); pass --force to delete anyway",
            live.len(),
            live.join(", ")
        );
    }

    if !yes && !confirm("delete these sessions?")? {
        println!("aborted");
        return Ok(());
    }

    for p in &plans {
        del::execute(dir, p).with_context(|| format!("deleting session {}", p.id))?;
    }
    println!("deleted {} session(s)", plans.len());
    Ok(())
}

fn matches_project(s: &SessionMeta, project: Option<&str>) -> bool {
    match project {
        None => true,
        Some(p) if p.eq_ignore_ascii_case("all") => true,
        Some(p) => {
            let needle = p.to_lowercase();
            s.project_slug.to_lowercase().contains(&needle)
                || s.cwd
                    .as_deref()
                    .is_some_and(|c| c.to_lowercase().contains(&needle))
        }
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// `30d`, `12h`, `2w`, `90m`.
pub fn parse_duration(spec: &str) -> Result<Duration> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(
        spec.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(spec.len()),
    );
    let n: i64 = num
        .parse()
        .with_context(|| format!("bad duration '{spec}', expected e.g. 30d"))?;
    let d = match unit {
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" | "" => Duration::days(n),
        "w" => Duration::weeks(n),
        other => bail!("unknown duration unit '{other}', use m/h/d/w"),
    };
    Ok(d)
}

/// `csb update` / `csb update --check`.
pub fn update(check_only: bool) -> Result<()> {
    if crate::update::checks_disabled() {
        println!("update checks are disabled (CSB_NO_UPDATE_CHECK is set)");
        return Ok(());
    }

    // Explicit command, so no throttle - always ask.
    let Some(found) = crate::update::check()? else {
        println!("csb {} is up to date", crate::update::CURRENT);
        return Ok(());
    };

    if check_only {
        println!(
            "csb {} is available (you have {})",
            found.version,
            crate::update::CURRENT
        );
        println!(
            "https://github.com/{}/{}/releases/tag/v{}",
            crate::update::REPO_OWNER,
            crate::update::REPO_NAME,
            found.version
        );
        println!("\nrun `csb update` to install it");
        return Ok(());
    }

    println!(
        "updating csb {} -> {}",
        crate::update::CURRENT,
        found.version
    );
    let status = crate::update::install(true)?;
    if status.is_updated() {
        println!("installed csb {} - restart csb to use it", status.version());
    } else {
        println!("csb {} is up to date", status.version());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30d").unwrap(), Duration::days(30));
        assert_eq!(parse_duration("2w").unwrap(), Duration::weeks(2));
        assert_eq!(parse_duration("90m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("7").unwrap(), Duration::days(7));
        assert!(parse_duration("7y").is_err());
        assert!(parse_duration("soon").is_err());
    }
}
