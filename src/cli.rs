use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use serde_json::json;
use std::io::{BufRead, BufWriter, Write};

use crate::del::{self, human_bytes};
use crate::index::{Index, SessionMeta, Sort};
use crate::paths::ClaudeDir;
use crate::transcript::{self, Event, LoadOpts};

// Table headers stay as args so they share the row format string's width specs.
#[allow(clippy::write_literal)]
pub fn list(index: &Index, project: Option<&str>, sort: Sort, as_json: bool) -> Result<()> {
    let mut sessions: Vec<&SessionMeta> = index
        .sessions
        .iter()
        .filter(|s| matches_project(s, project))
        .collect();
    sort.apply(&mut sessions);

    // Every stdout write goes through `writeln!` so a closed pipe (`csb list |
    // head`) is an io error main can turn into a clean exit, not a panic.
    let mut out = BufWriter::new(std::io::stdout().lock());

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
                    // Lossy, not the `PathBuf`: `json!` unwraps a conversion
                    // that fails on a non-UTF-8 path.
                    "path": s.path.to_string_lossy(),
                    "sizeBytes": s.size_bytes,
                    "lastActivity": s.activity().to_rfc3339(),
                    "userMessages": s.user_msgs,
                    "assistantMessages": s.assistant_msgs,
                    "toolCalls": s.tool_calls,
                })
            })
            .collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
        out.flush()?;
        return Ok(());
    }

    if sessions.is_empty() {
        writeln!(out, "no sessions found")?;
        out.flush()?;
        return Ok(());
    }

    let total: u64 = sessions.iter().map(|s| s.size_bytes).sum();
    writeln!(
        out,
        "{:<8}  {:<16}  {:>6}  {:>9}  {}",
        "ID", "LAST ACTIVITY", "MSGS", "SIZE", "TITLE"
    )?;
    for s in &sessions {
        writeln!(
            out,
            "{:<8}  {:<16}  {:>6}  {:>9}  {}",
            s.short_id(),
            s.activity()
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            s.user_msgs + s.assistant_msgs,
            human_bytes(s.size_bytes),
            crate::index::truncate(&s.title, 70),
        )?;
    }
    writeln!(out, "\n{} sessions, {}", sessions.len(), human_bytes(total))?;
    out.flush()?;
    Ok(())
}

// Table headers stay as args so they share the row format string's width specs.
#[allow(clippy::write_literal)]
pub fn projects(index: &Index) -> Result<()> {
    let projects = index.projects();
    let mut out = BufWriter::new(std::io::stdout().lock());
    if projects.is_empty() {
        writeln!(out, "no projects found")?;
        out.flush()?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<6}  {:>9}  {:<40}  {}",
        "SESS", "SIZE", "SLUG", "LOCATION"
    )?;
    for p in &projects {
        writeln!(
            out,
            "{:<6}  {:>9}  {:<40}  {}",
            p.count,
            human_bytes(p.bytes),
            crate::index::truncate(&p.slug, 40),
            p.label,
        )?;
    }
    out.flush()?;
    Ok(())
}

pub fn show(index: &Index, needle: &str, raw: bool, sidechains: bool) -> Result<()> {
    let meta = index.find(needle)?;
    if raw {
        // Streamed as bytes: a transcript is not guaranteed to be valid UTF-8
        // end to end, and `--raw` promises the file as it is.
        let mut file = std::fs::File::open(&meta.path)?;
        std::io::copy(&mut file, &mut std::io::stdout().lock())?;
        return Ok(());
    }

    let mut out = BufWriter::new(std::io::stdout().lock());
    writeln!(out, "# {}", meta.title)?;
    writeln!(out, "  id       {}", meta.id)?;
    writeln!(out, "  cwd      {}", meta.location())?;
    if let Some(b) = &meta.git_branch {
        writeln!(out, "  branch   {b}")?;
    }
    writeln!(
        out,
        "  activity {}",
        meta.activity()
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
    )?;
    writeln!(
        out,
        "  volume   {} messages, {} tool calls, {}",
        meta.user_msgs + meta.assistant_msgs,
        meta.tool_calls,
        human_bytes(meta.size_bytes)
    )?;
    writeln!(out)?;

    let opts = LoadOpts {
        max_entries: usize::MAX,
        include_sidechains: sidechains,
    };
    let t = transcript::load(&meta.path, &opts)?;
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
        }
    }
    out.flush()?;
    Ok(())
}

/// The sessions `delete` would act on: every explicit id or prefix, plus
/// everything idle since `now - older_than`, each session only once.
fn select<'a>(
    index: &'a Index,
    ids: &[String],
    older_than: Option<&str>,
    project: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Vec<&'a SessionMeta>> {
    let mut targets: Vec<&SessionMeta> = Vec::new();

    for needle in ids {
        let meta = index.find(needle)?;
        if !targets.iter().any(|t| t.id == meta.id) {
            targets.push(meta);
        }
    }

    if let Some(spec) = older_than {
        let cutoff = now - parse_duration(spec)?;
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

    Ok(targets)
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
    let targets = select(index, ids, older_than, project, Utc::now())?;
    let mut out = BufWriter::new(std::io::stdout().lock());

    if targets.is_empty() {
        writeln!(out, "nothing matched")?;
        out.flush()?;
        return Ok(());
    }

    let plans: Vec<_> = targets.iter().map(|m| del::plan(dir, m)).collect();
    let total: u64 = plans.iter().map(|p| p.bytes).sum();

    for p in &plans {
        let flag = if p.recent { "  [ACTIVE?]" } else { "" };
        writeln!(out, "{} {}{flag}", p.short_id(), p.title)?;
        for path in &p.paths {
            writeln!(out, "    {}", path.display())?;
        }
    }
    writeln!(
        out,
        "\n{} session(s), {} to the recycle bin",
        plans.len(),
        human_bytes(total)
    )?;
    // The plan has to be on screen before `confirm` asks about it, and before
    // any of the refusals below reach stderr.
    out.flush()?;

    if dry_run {
        writeln!(out, "(dry run - nothing deleted)")?;
        out.flush()?;
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

    // Non-zero exit: a script that asked for a delete and got none did not
    // succeed.
    if !yes && !confirm("delete these sessions?")? {
        bail!("aborted");
    }

    let outcome = del::execute_all(dir, &plans);
    writeln!(out, "{}", outcome.summary(total))?;
    out.flush()?;
    match outcome.failed {
        // Already reported above, but the exit status has to say so too.
        Some(e) => Err(e),
        None => Ok(()),
    }
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
    // EOF, not a "no": a cron job with stdin closed must fail loudly rather
    // than report a clean run in which nothing was deleted.
    if std::io::stdin().lock().read_line(&mut answer)? == 0 {
        bail!("no terminal to confirm on; pass --yes");
    }
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
    // try_* rather than the plain constructors: those panic on overflow, and
    // the number here comes straight from the command line.
    let d = match unit {
        "m" => Duration::try_minutes(n),
        "h" => Duration::try_hours(n),
        "d" | "" => Duration::try_days(n),
        "w" => Duration::try_weeks(n),
        other => bail!("unknown duration unit '{other}', use m/h/d/w"),
    };
    d.with_context(|| format!("duration '{spec}' is out of range"))
}

/// `csb update` / `csb update --check` / `csb update --force`.
pub fn update(check_only: bool, force: bool) -> Result<()> {
    // The opt-out covers *checks*; an explicit `csb update` is the user asking
    // for the install and still goes ahead.
    if check_only && crate::update::checks_disabled() {
        println!("update checks are disabled (CSB_NO_UPDATE_CHECK is set)");
        return Ok(());
    }

    // --force reinstalls whatever the latest release holds, without asking
    // whether this binary looks current. That is the way out of a half-applied
    // update: the running binary can be up to date while a sibling it ships
    // with is not, and nothing else would notice.
    if force && !check_only {
        println!("reinstalling the latest release");
        let status = crate::update::install(true, true)?;
        println!("installed csb {} - restart csb to use it", status.version());
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
        if crate::update::package_managed() {
            println!("\n{}", crate::update::PACKAGE_MANAGED_HINT);
        } else {
            println!("\nrun `csb update` to install it");
        }
        return Ok(());
    }

    println!(
        "updating csb {} -> {}",
        crate::update::CURRENT,
        found.version
    );
    let status = crate::update::install(true, false)?;
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
    use std::path::PathBuf;

    /// A fixed "now", so `--older-than` cutoffs are not wall-clock dependent.
    fn now() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn meta(id: &str, slug: &str, idle: Duration) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            path: PathBuf::new(),
            project_slug: slug.into(),
            size_bytes: 0,
            modified_ms: 0,
            first_ts: None,
            last_ts: Some(now() - idle),
            title: String::new(),
            cwd: None,
            git_branch: None,
            user_msgs: 0,
            assistant_msgs: 0,
            tool_calls: 0,
        }
    }

    fn index(sessions: Vec<SessionMeta>) -> Index {
        Index {
            sessions,
            warnings: Vec::new(),
        }
    }

    fn ids_of(targets: &[&SessionMeta]) -> Vec<String> {
        targets.iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn explicit_ids_resolve_and_dedupe() {
        let ix = index(vec![
            meta("aaaa1111", "p", Duration::days(1)),
            meta("bbbb2222", "p", Duration::days(1)),
        ]);
        // Full id and a prefix of the same session count once.
        let ids = ["aaaa1111".to_string(), "aaa".to_string()];
        let got = select(&ix, &ids, None, None, now()).unwrap();
        assert_eq!(ids_of(&got), ["aaaa1111"]);
    }

    #[test]
    fn older_than_unions_with_explicit_ids() {
        let ix = index(vec![
            meta("old1", "p", Duration::days(40)),
            meta("old2", "p", Duration::days(40)),
            meta("new1", "p", Duration::hours(1)),
        ]);
        let ids = ["old1".to_string(), "new1".to_string()];
        let got = select(&ix, &ids, Some("30d"), None, now()).unwrap();
        // Explicit ids keep their order, the age sweep appends what is new.
        assert_eq!(ids_of(&got), ["old1", "new1", "old2"]);
    }

    #[test]
    fn older_than_respects_the_project() {
        let ix = index(vec![
            meta("a1", "-src-alpha", Duration::days(40)),
            meta("b1", "-src-beta", Duration::days(40)),
        ]);
        let got = select(&ix, &[], Some("30d"), Some("alpha"), now()).unwrap();
        assert_eq!(ids_of(&got), ["a1"]);
        let all = select(&ix, &[], Some("30d"), Some("all"), now()).unwrap();
        assert_eq!(ids_of(&all), ["a1", "b1"]);
    }

    #[test]
    fn neither_ids_nor_older_than_is_an_error() {
        let ix = index(vec![meta("a1", "p", Duration::days(1))]);
        let e = select(&ix, &[], None, None, now()).unwrap_err().to_string();
        assert!(e.contains("give session ids"), "{e}");
    }

    #[test]
    fn an_empty_id_is_refused() {
        let ix = index(vec![meta("a1", "p", Duration::days(1))]);
        let e = select(&ix, &[String::new()], None, None, now())
            .unwrap_err()
            .to_string();
        assert!(e.contains("empty session id"), "{e}");
    }

    #[test]
    fn an_ambiguous_prefix_is_refused() {
        let ix = index(vec![
            meta("aaa1", "p", Duration::days(1)),
            meta("aaa2", "p", Duration::days(1)),
        ]);
        let e = select(&ix, &["aaa".to_string()], None, None, now())
            .unwrap_err()
            .to_string();
        assert!(e.contains("matches"), "{e}");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30d").unwrap(), Duration::days(30));
        assert_eq!(parse_duration("2w").unwrap(), Duration::weeks(2));
        assert_eq!(parse_duration("90m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("7").unwrap(), Duration::days(7));
        assert!(parse_duration("7y").is_err());
        assert!(parse_duration("soon").is_err());
        // Overflow is an error, not a panic: chrono's Duration::weeks aborts on
        // anything this large.
        assert!(parse_duration("9223372036854775807w").is_err());
        assert!(parse_duration("9223372036854775807d").is_err());
    }
}
