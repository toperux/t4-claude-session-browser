use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use memchr::memmem;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::{slug_hint, ClaudeDir};

/// A session that is younger than this is probably still being written to.
pub const RECENT_SECS: i64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub path: PathBuf,
    pub project_slug: String,
    pub size_bytes: u64,
    pub modified_ms: i64,
    pub first_ts: Option<DateTime<Utc>>,
    pub last_ts: Option<DateTime<Utc>>,
    pub title: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub user_msgs: u32,
    pub assistant_msgs: u32,
    pub tool_calls: u32,
}

/// First 8 characters of an id. Ids come from file stems, not validated UUIDs,
/// so this must survive short and non-ASCII names rather than slicing bytes.
pub fn short_id(id: &str) -> &str {
    let end = id.char_indices().nth(8).map_or(id.len(), |(i, _)| i);
    &id[..end]
}

impl SessionMeta {
    pub fn short_id(&self) -> &str {
        short_id(&self.id)
    }

    /// Last real activity: the newest record timestamp, else the file mtime.
    /// An mtime outside chrono's range (a bad clock, a mangled archive) sorts
    /// to the epoch rather than taking the whole index down with it.
    pub fn activity(&self) -> DateTime<Utc> {
        self.last_ts.unwrap_or_else(|| {
            Utc.timestamp_millis_opt(self.modified_ms)
                .single()
                .unwrap_or(DateTime::UNIX_EPOCH)
        })
    }

    /// Likely a live session someone is using right now.
    pub fn is_recent(&self) -> bool {
        (Utc::now() - self.activity()).num_seconds() < RECENT_SECS
    }

    pub fn location(&self) -> String {
        self.cwd
            .clone()
            .unwrap_or_else(|| slug_hint(&self.project_slug))
    }
}

/// One project directory plus the sessions inside it.
#[derive(Debug, Clone)]
pub struct Project {
    pub slug: String,
    pub label: String,
    pub sessions: Vec<SessionMeta>,
}

impl Project {
    pub fn size_bytes(&self) -> u64 {
        self.sessions.iter().map(|s| s.size_bytes).sum()
    }
}

#[derive(Debug, Clone)]
pub struct Index {
    pub sessions: Vec<SessionMeta>,
    /// Files that could not be read. Returned rather than printed: the TUI owns
    /// the screen, and stray stderr writes corrupt the frame.
    pub warnings: Vec<String>,
}

impl Index {
    /// Scan every session file, reusing cached metadata where size+mtime match.
    pub fn build(dir: &ClaudeDir) -> Result<Self> {
        let files = discover(dir)?;
        let cache = Cache::load();

        let scanned: Vec<Result<SessionMeta, String>> = files
            .par_iter()
            .map(|(slug, path)| {
                let stat =
                    std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let size = stat.len();
                let modified_ms = stat.modified().map(to_millis).unwrap_or(0);
                if let Some(hit) = cache.get(path, size, modified_ms) {
                    return Ok(hit);
                }
                scan_file(slug, path, size, modified_ms)
                    .map_err(|e| format!("{}: {e}", path.display()))
            })
            .collect();

        let mut sessions = Vec::with_capacity(scanned.len());
        let mut warnings = Vec::new();
        for result in scanned {
            match result {
                Ok(meta) => sessions.push(meta),
                Err(w) => warnings.push(w),
            }
        }

        sessions.sort_by_key(|s| Reverse(s.activity()));
        Cache::store(&sessions);
        Ok(Self { sessions, warnings })
    }

    /// One-line summary of any unreadable files, for a status bar.
    pub fn warning_summary(&self) -> Option<String> {
        let first = self.warnings.first()?;
        Some(match self.warnings.len() {
            1 => format!("skipped 1 unreadable file ({first})"),
            n => format!("skipped {n} unreadable files (first: {first})"),
        })
    }

    /// Sessions grouped by project, newest project first.
    pub fn projects(&self) -> Vec<Project> {
        let mut by_slug: HashMap<&str, Vec<SessionMeta>> = HashMap::new();
        for s in &self.sessions {
            by_slug.entry(&s.project_slug).or_default().push(s.clone());
        }
        let mut out: Vec<Project> = by_slug
            .into_iter()
            .map(|(slug, sessions)| {
                let label = sessions
                    .iter()
                    .find_map(|s| s.cwd.clone())
                    .unwrap_or_else(|| slug_hint(slug));
                Project {
                    slug: slug.to_string(),
                    label,
                    sessions,
                }
            })
            .collect();
        out.sort_by_key(|p| Reverse(p.sessions.first().map(|s| s.activity())));
        out
    }

    /// Resolve a full id or unique id prefix.
    pub fn find(&self, needle: &str) -> Result<&SessionMeta> {
        let hits: Vec<&SessionMeta> = self
            .sessions
            .iter()
            .filter(|s| s.id.starts_with(needle))
            .collect();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => anyhow::bail!("no session matches '{needle}'"),
            n => {
                let sample: Vec<&str> = hits.iter().take(5).map(|s| s.id.as_str()).collect();
                anyhow::bail!("'{needle}' matches {n} sessions: {}", sample.join(", "))
            }
        }
    }
}

fn discover(dir: &ClaudeDir) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let projects = dir.projects();
    for entry in
        std::fs::read_dir(&projects).with_context(|| format!("reading {}", projects.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().into_owned();
        for f in std::fs::read_dir(entry.path())? {
            let f = f?;
            let path = f.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                out.push((slug.clone(), path));
            }
        }
    }
    Ok(out)
}

fn to_millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- scanning

/// One buffered pass per file. Lines are classified with substring searches;
/// only title candidates and the first message line are handed to serde_json.
fn scan_file(slug: &str, path: &Path, size_bytes: u64, modified_ms: i64) -> Result<SessionMeta> {
    let f_type = memmem::Finder::new(br#""type""#);
    let f_meta = memmem::Finder::new(br#""isMeta""#);
    let f_side = memmem::Finder::new(br#""isSidechain""#);
    let f_ts = memmem::Finder::new(br#""timestamp""#);

    let mut custom_title: Option<String> = None;
    let mut agent_name: Option<String> = None;
    let mut prompts: Vec<String> = Vec::new();
    let mut first_ts = None;
    let mut last_ts = None;
    let mut cwd = None;
    let mut git_branch = None;
    let mut probes_left = 5u8;
    let (mut user_msgs, mut assistant_msgs, mut tool_calls) = (0u32, 0u32, 0u32);

    let reader = BufReader::with_capacity(256 * 1024, File::open(path)?);
    for line in reader.split(b'\n') {
        let line = line?;
        if line.is_empty() {
            continue;
        }

        if let Some(ts) = string_field(&line, &f_ts).and_then(parse_ts) {
            first_ts.get_or_insert(ts);
            last_ts = Some(ts);
        }

        let (mut is_user, mut is_asst) = (false, false);
        let (mut is_result, mut is_custom, mut is_agent) = (false, false, false);
        for_each_type(&line, &f_type, |value| match value {
            "user" => is_user = true,
            "assistant" => is_asst = true,
            "tool_use" => tool_calls += 1,
            "tool_result" => is_result = true,
            "custom-title" => is_custom = true,
            "agent-name" => is_agent = true,
            _ => {}
        });

        if is_asst {
            assistant_msgs += 1;
        } else if is_user && !is_result {
            user_msgs += 1;
        }

        // Fill each field once, from whichever early message carries it - a
        // later record missing `gitBranch` must not erase an earlier one. The
        // probe budget keeps this from degrading into a full parse of every
        // message line when a transcript never records `cwd` at all.
        if (is_user || is_asst) && probes_left > 0 && (cwd.is_none() || git_branch.is_none()) {
            probes_left -= 1;
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                if cwd.is_none() {
                    cwd = v["cwd"].as_str().map(str::to_string);
                }
                if git_branch.is_none() {
                    git_branch = v["gitBranch"].as_str().map(str::to_string);
                }
            }
        }

        if is_custom {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                custom_title = v["customTitle"].as_str().map(str::to_string);
            }
        } else if is_agent {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                agent_name = v["agentName"].as_str().map(str::to_string);
            }
        } else if is_user
            && prompts.len() < 4
            && !is_result
            && !flag_is_true(&line, &f_meta)
            && !flag_is_true(&line, &f_side)
        {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                if let Some(text) = message_text(&v["message"]["content"]) {
                    prompts.push(text);
                }
            }
        }
    }

    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = derive_title(&custom_title, &agent_name, &prompts).unwrap_or_else(|| {
        if user_msgs + assistant_msgs == 0 {
            "(empty session)".to_string()
        } else {
            format!("(untitled {})", short_id(&id))
        }
    });

    Ok(SessionMeta {
        id,
        path: path.to_path_buf(),
        project_slug: slug.to_string(),
        size_bytes,
        modified_ms,
        first_ts,
        last_ts,
        title,
        cwd,
        git_branch,
        user_msgs,
        assistant_msgs,
        tool_calls,
    })
}

/// Visit the value of every `"type"` key in a line - the record's own type and
/// any nested content-block types.
///
/// Scanning all of them rather than just the first keeps this independent of
/// key order, which matters: `attachment` records carry a nested `"type"`
/// *before* their own. It is also safe against text that merely looks like
/// JSON, because a quote inside a JSON string is escaped as `\"`, so the byte
/// sequence `"type"` can never occur inside a string value.
fn for_each_type(line: &[u8], finder: &memmem::Finder, mut visit: impl FnMut(&str)) {
    let mut pos = 0;
    while let Some(off) = finder.find(&line[pos..]) {
        pos += off + finder.needle().len();
        if let Some(value) = read_value_after_key(&line[pos..]) {
            visit(value);
        }
    }
}

/// The string value of `key`, tolerating whitespace around the colon.
fn string_field<'a>(line: &'a [u8], finder: &memmem::Finder) -> Option<&'a str> {
    let at = finder.find(line)? + finder.needle().len();
    read_value_after_key(&line[at..])
}

/// True when `key` is present with the literal value `true`.
fn flag_is_true(line: &[u8], finder: &memmem::Finder) -> bool {
    let Some(at) = finder.find(line) else {
        return false;
    };
    let rest = &line[at + finder.needle().len()..];
    let rest = skip_colon(rest).unwrap_or(rest);
    rest.starts_with(b"true")
}

/// Given the bytes just after a key, read its quoted string value.
fn read_value_after_key(rest: &[u8]) -> Option<&str> {
    let rest = skip_colon(rest)?;
    let rest = rest.strip_prefix(b"\"")?;
    let end = memchr::memchr(b'"', rest)?;
    std::str::from_utf8(&rest[..end]).ok()
}

fn skip_colon(rest: &[u8]) -> Option<&[u8]> {
    let rest = rest.strip_prefix(b":").or_else(|| {
        let trimmed = rest.iter().position(|b| !b.is_ascii_whitespace())?;
        rest[trimmed..].strip_prefix(b":")
    })?;
    let start = rest.iter().position(|b| !b.is_ascii_whitespace())?;
    Some(&rest[start..])
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Concatenate the `text` of a message content value (string or block array).
pub fn message_text(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let blocks = content.as_array()?;
    let joined: Vec<&str> = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join("\n"))
    }
}

// ---------------------------------------------------------------- titles

const CONTINUED: &str = "This session is being continued from a previous conversation";

/// custom-title > agent-name > first usable user prompt.
pub fn derive_title(
    custom: &Option<String>,
    agent: &Option<String>,
    prompts: &[String],
) -> Option<String> {
    for candidate in [custom, agent].into_iter().flatten() {
        let candidate = candidate.trim();
        if !candidate.is_empty() {
            return Some(truncate(candidate, 110));
        }
    }

    let mut continued = false;
    // A bare `/clear` or `/login` says nothing about the session; keep it only
    // if no later prompt has real content.
    let mut weak: Option<String> = None;

    for raw in prompts {
        if raw.trim_start().starts_with(CONTINUED) {
            continued = true;
            continue;
        }
        let Some(clean) = clean_prompt(raw) else {
            continue;
        };
        if is_bare_command(&clean) {
            weak.get_or_insert(clean);
            continue;
        }
        return Some(if continued {
            truncate(&format!("(continued) {clean}"), 110)
        } else {
            truncate(&clean, 110)
        });
    }

    weak.or_else(|| continued.then(|| "(continued session)".to_string()))
}

fn is_bare_command(s: &str) -> bool {
    s.starts_with('/') && !s.contains(char::is_whitespace)
}

/// Turn a raw first prompt into something readable, or None if it carries no
/// signal (pure command noise, reminders, empty).
pub fn clean_prompt(raw: &str) -> Option<String> {
    let mut s = raw.to_string();

    // A slash command carries its name and args in tags; use those directly.
    if let Some(name) = tag_value(&s, "command-name") {
        let args = tag_value(&s, "command-args").unwrap_or_default();
        let joined = format!("{name} {args}");
        let joined = joined.trim();
        return (!joined.is_empty()).then(|| squash(joined));
    }

    s = strip_noise_tags(&s);

    // "Caveat: ..." preambles are boilerplate; drop that paragraph.
    let s = s.trim_start();
    let s = if s.starts_with("Caveat:") {
        s.split_once("\n\n").map(|(_, rest)| rest).unwrap_or("")
    } else {
        s
    };

    let s = squash(s);
    (!s.is_empty()).then_some(s)
}

/// Boilerplate the CLI wraps around prompts; it says nothing about the session.
pub fn strip_noise_tags(s: &str) -> String {
    let mut out = s.to_string();
    for tag in [
        "system-reminder",
        "local-command-stdout",
        "local-command-caveat",
        "command-message",
    ] {
        out = strip_tag_blocks(&out, tag);
    }
    out
}

pub fn tag_value(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(s[start..end].trim().to_string())
}

fn strip_tag_blocks(s: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => return out, // unterminated: drop the tail
        }
    }
    out.push_str(rest);
    out
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

// ---------------------------------------------------------------- cache

/// Bump whenever what gets indexed changes. Without this a metadata fix would
/// appear to do nothing: entries are keyed on size+mtime, which do not change
/// when the *indexing* does, so stale values would be served indefinitely.
const CACHE_SCHEMA: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    schema: u32,
    entries: HashMap<String, SessionMeta>,
}

impl Cache {
    fn file() -> Option<PathBuf> {
        Some(
            dirs::cache_dir()?
                .join("claude-session-browser")
                .join("index.json"),
        )
    }

    fn load() -> Self {
        let Some(path) = Self::file() else {
            return Self::default();
        };
        let cache: Self = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if cache.schema == CACHE_SCHEMA {
            cache
        } else {
            Self::default()
        }
    }

    fn get(&self, path: &Path, size: u64, modified_ms: i64) -> Option<SessionMeta> {
        let hit = self.entries.get(&path.to_string_lossy().into_owned())?;
        (hit.size_bytes == size && hit.modified_ms == modified_ms).then(|| hit.clone())
    }

    fn store(sessions: &[SessionMeta]) {
        let Some(path) = Self::file() else { return };
        let cache = Cache {
            schema: CACHE_SCHEMA,
            entries: sessions
                .iter()
                .map(|s| (s.path.to_string_lossy().into_owned(), s.clone()))
                .collect(),
        };
        let Ok(json) = serde_json::to_vec(&cache) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(prompts: &[&str]) -> Option<String> {
        let owned: Vec<String> = prompts.iter().map(|s| s.to_string()).collect();
        derive_title(&None, &None, &owned)
    }

    #[test]
    fn short_id_survives_odd_file_stems() {
        assert_eq!(short_id("00dc9e25-828a-4dc1"), "00dc9e25");
        // Stems are not validated UUIDs: a stray `notes.jsonl` must not panic.
        assert_eq!(short_id("ab"), "ab");
        assert_eq!(short_id(""), "");
        // Truncating on a char boundary, not a byte one.
        assert_eq!(short_id("日本語テストです"), "日本語テストです");
        assert_eq!(short_id("日本語テストですね"), "日本語テストです");
    }

    /// The untitled fallback puts the file stem in the title. Stems are not
    /// validated UUIDs, so slicing one by bytes would panic mid-codepoint - and
    /// this runs inside `Index::build`, which would take the GUI down silently.
    #[test]
    fn untitled_fallback_survives_a_non_ascii_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("セッション記録.jsonl");
        std::fs::write(&path, br#"{"type":"user","message":{"content":""}}"#).unwrap();

        let meta = scan_file("slug", &path, 40, 0).unwrap();
        assert_eq!(meta.title, "(untitled セッション記録)");
    }

    /// A file whose mtime is far outside chrono's range must not panic the
    /// whole index; it just sorts to the epoch.
    #[test]
    fn absurd_mtime_does_not_panic() {
        let meta = SessionMeta {
            id: "x".into(),
            path: PathBuf::new(),
            project_slug: "s".into(),
            size_bytes: 0,
            modified_ms: i64::MAX,
            first_ts: None,
            last_ts: None,
            title: String::new(),
            cwd: None,
            git_branch: None,
            user_msgs: 0,
            assistant_msgs: 0,
            tool_calls: 0,
        };
        assert_eq!(meta.activity(), DateTime::UNIX_EPOCH);
    }

    #[test]
    fn custom_title_wins() {
        let title = derive_title(
            &Some("PR task/952399-HARDENING".into()),
            &Some("package and lock".into()),
            &["something else".into()],
        );
        assert_eq!(title.unwrap(), "PR task/952399-HARDENING");
    }

    #[test]
    fn agent_name_is_second() {
        let title = derive_title(&None, &Some("package and lock".into()), &[]);
        assert_eq!(title.unwrap(), "package and lock");
    }

    #[test]
    fn slash_command_prompt() {
        let raw = "<command-message>statusline</command-message>\n<command-name>/statusline</command-name>\n<command-args>can we use U+2387 for the branch icon?</command-args>";
        assert_eq!(
            t(&[raw]).unwrap(),
            "/statusline can we use U+2387 for the branch icon?"
        );
    }

    #[test]
    fn bare_slash_command_without_args() {
        let raw = "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>";
        assert_eq!(t(&[raw]).unwrap(), "/clear");
    }

    #[test]
    fn continued_session_uses_next_prompt() {
        let raw = format!("{CONTINUED}. The conversation is summarized below: blah");
        assert_eq!(
            t(&[&raw, "now fix the parser"]).unwrap(),
            "(continued) now fix the parser"
        );
    }

    #[test]
    fn continued_session_alone() {
        let raw = format!("{CONTINUED}. blah blah");
        assert_eq!(t(&[&raw]).unwrap(), "(continued session)");
    }

    #[test]
    fn bare_command_loses_to_a_later_real_prompt() {
        let clear = "<command-name>/clear</command-name><command-args></command-args>";
        assert_eq!(
            t(&[clear, "when opening a new file the window hides"]).unwrap(),
            "when opening a new file the window hides"
        );
        // ...but is still better than nothing.
        assert_eq!(t(&[clear]).unwrap(), "/clear");
        // A command *with* args is already descriptive - it wins outright.
        let recap = "<command-name>/recap</command-name><command-args>the auth work</command-args>";
        assert_eq!(t(&[recap, "later prompt"]).unwrap(), "/recap the auth work");
    }

    #[test]
    fn plain_prompt_is_squashed_and_truncated() {
        let raw = "I want to be able to curate my   claude code sessions.\n\nwrite me an app";
        assert_eq!(
            t(&[raw]).unwrap(),
            "I want to be able to curate my claude code sessions. write me an app"
        );
    }

    #[test]
    fn reminders_are_stripped() {
        let raw = "<system-reminder>ignore me</system-reminder>real content here";
        assert_eq!(t(&[raw]).unwrap(), "real content here");
    }

    #[test]
    fn empty_prompts_yield_no_title() {
        assert!(t(&["   ", "<system-reminder>only noise</system-reminder>"]).is_none());
        assert!(derive_title(&Some("  ".into()), &None, &[]).is_none());
    }

    #[test]
    fn message_text_handles_both_shapes() {
        let s = serde_json::json!("hello");
        assert_eq!(message_text(&s).unwrap(), "hello");
        let blocks = serde_json::json!([
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "hi"},
        ]);
        assert_eq!(message_text(&blocks).unwrap(), "hi");
        let only_tools = serde_json::json!([{"type": "tool_use", "name": "Bash"}]);
        assert!(message_text(&only_tools).is_none());
    }

    #[test]
    fn timestamp_extraction() {
        let finder = memmem::Finder::new(br#""timestamp""#);
        let line = br#"{"type":"user","timestamp":"2026-08-23T18:48:01.103Z","uuid":"x"}"#;
        let ts = string_field(line, &finder).and_then(parse_ts).unwrap();
        assert_eq!(ts.to_rfc3339(), "2026-08-23T18:48:01.103+00:00");
        assert!(string_field(b"{}", &finder).is_none());
    }

    #[test]
    fn type_scan_is_whitespace_and_order_tolerant() {
        let finder = memmem::Finder::new(br#""type""#);
        let mut seen = Vec::new();
        // Pretty-printed, and with the record's own type *after* a nested one -
        // exactly the shape `attachment` records use.
        let line = br#"{"attachment": {"type" : "delta"}, "type": "attachment"}"#;
        for_each_type(line, &finder, |v| seen.push(v.to_string()));
        assert_eq!(seen, ["delta", "attachment"]);

        // Multiple tool_use blocks in one assistant line are all counted.
        let mut tools = 0;
        let line = br#"{"type":"assistant","message":{"content":[{"type":"tool_use"},{"type":"tool_use"}]}}"#;
        for_each_type(line, &finder, |v| {
            if v == "tool_use" {
                tools += 1
            }
        });
        assert_eq!(tools, 2);
    }

    #[test]
    fn bool_flags_tolerate_spacing() {
        let finder = memmem::Finder::new(br#""isMeta""#);
        assert!(flag_is_true(br#"{"isMeta":true}"#, &finder));
        assert!(flag_is_true(br#"{"isMeta" : true}"#, &finder));
        assert!(!flag_is_true(br#"{"isMeta":false}"#, &finder));
        assert!(!flag_is_true(br#"{"other":true}"#, &finder));
    }
}
