use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use memchr::memmem;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::{is_session_id, ClaudeDir};

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

    /// Likely a live session someone is using right now. The file mtime counts
    /// as well as the last record timestamp: a session whose tail records carry
    /// no `timestamp` is still being written to.
    pub fn is_recent(&self) -> bool {
        let mtime = Utc
            .timestamp_millis_opt(self.modified_ms)
            .single()
            .unwrap_or(DateTime::UNIX_EPOCH);
        let touched = self.activity().max(mtime);
        (Utc::now() - touched).num_seconds() < RECENT_SECS
    }

    /// Slugification maps every separator to `-`, so it cannot be reversed -
    /// with no recorded `cwd` the slug is shown as-is rather than inventing a
    /// path that looks real but isn't.
    pub fn location(&self) -> String {
        self.cwd
            .clone()
            .unwrap_or_else(|| self.project_slug.clone())
    }
}

/// Session orderings; every one is descending.
#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum Sort {
    Date,
    Size,
    Msgs,
}

impl Sort {
    pub fn apply<S: Borrow<SessionMeta>>(self, list: &mut [S]) {
        match self {
            Sort::Date => list.sort_by_key(|s| Reverse(s.borrow().activity())),
            Sort::Size => list.sort_by_key(|s| Reverse(s.borrow().size_bytes)),
            Sort::Msgs => {
                list.sort_by_key(|s| Reverse(s.borrow().user_msgs + s.borrow().assistant_msgs))
            }
        }
    }
}

/// One project directory, summarised.
#[derive(Debug, Clone)]
pub struct Project {
    pub slug: String,
    /// The recorded `cwd`, else the slug (see `SessionMeta::location`).
    pub label: String,
    pub count: usize,
    pub bytes: u64,
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
        let Discovered {
            files,
            mut warnings,
        } = discover(dir)?;
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
        for result in scanned {
            match result {
                Ok(meta) => sessions.push(meta),
                Err(w) => warnings.push(w),
            }
        }

        sessions.sort_by_key(|s| Reverse(s.activity()));
        cache.store(&sessions, &dir.projects());
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

    /// Projects, newest first. `sessions` is already newest-first, so the order
    /// each slug is first seen in is the order wanted.
    pub fn projects(&self) -> Vec<Project> {
        let mut out: Vec<Project> = Vec::new();
        let mut at: HashMap<&str, usize> = HashMap::new();
        for s in &self.sessions {
            let i = *at.entry(&s.project_slug).or_insert_with(|| {
                out.push(Project {
                    slug: s.project_slug.clone(),
                    label: String::new(),
                    count: 0,
                    bytes: 0,
                });
                out.len() - 1
            });
            let p = &mut out[i];
            p.count += 1;
            p.bytes += s.size_bytes;
            if p.label.is_empty() {
                p.label = s.cwd.clone().unwrap_or_default();
            }
        }
        for p in &mut out {
            if p.label.is_empty() {
                p.label = p.slug.clone();
            }
        }
        out
    }

    /// Sessions in project `slug` (all when `None`) whose title, location or
    /// id prefix contains `needle`, sorted.
    pub fn filter(&self, slug: Option<&str>, needle: &str, sort: Sort) -> Vec<SessionMeta> {
        let needle = needle.to_lowercase();
        let mut list: Vec<SessionMeta> = self
            .sessions
            .iter()
            .filter(|s| slug.is_none_or(|sl| s.project_slug == sl))
            .filter(|s| {
                needle.is_empty()
                    || s.title.to_lowercase().contains(&needle)
                    || s.location().to_lowercase().contains(&needle)
                    || s.id.starts_with(&needle)
            })
            .cloned()
            .collect();
        sort.apply(&mut list);
        list
    }

    /// The `marked` sessions, or `fallback` alone when nothing is marked.
    pub fn marked_or(
        &self,
        marked: &HashSet<String>,
        fallback: Option<&SessionMeta>,
    ) -> Vec<SessionMeta> {
        if marked.is_empty() {
            return fallback.cloned().into_iter().collect();
        }
        self.sessions
            .iter()
            .filter(|s| marked.contains(&s.id))
            .cloned()
            .collect()
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

/// Session files plus a warning for every project directory that could not be
/// read. Only the top-level `projects/` listing is fatal: one unreadable
/// subdirectory must not take the whole index down.
struct Discovered {
    /// `(project slug, transcript path)` pairs.
    files: Vec<(String, PathBuf)>,
    warnings: Vec<String>,
}

fn discover(dir: &ClaudeDir) -> Result<Discovered> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let projects = dir.projects();
    for entry in
        std::fs::read_dir(&projects).with_context(|| format!("reading {}", projects.display()))?
    {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().into_owned();
        let files = match std::fs::read_dir(entry.path()) {
            Ok(files) => files,
            Err(e) => {
                warnings.push(format!("{}: {e}", entry.path().display()));
                continue;
            }
        };
        for f in files.flatten() {
            let path = f.path();
            let is_session = path.extension().is_some_and(|e| e == "jsonl")
                && path
                    .file_stem()
                    .is_some_and(|s| is_session_id(&s.to_string_lossy()));
            if is_session {
                out.push((slug.clone(), path));
            }
        }
    }
    Ok(Discovered {
        files: out,
        warnings,
    })
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

        // Meta lines (the local-command caveat, injected reminders) are CLI
        // scaffolding, not messages - counting them would hide `(empty session)`.
        if is_asst {
            assistant_msgs += 1;
        } else if is_user && !is_result && !flag_is_true(&line, &f_meta) {
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

/// O(max), not O(len): the GUI calls this on every visible entry every frame,
/// and a full `chars().count()` over a 12k-char turn adds up.
pub fn truncate(s: &str, max: usize) -> String {
    if s.char_indices().nth(max).is_none() {
        return s.to_string();
    }
    let keep = s
        .char_indices()
        .nth(max.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    format!("{}…", s[..keep].trim_end())
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

    /// Entries under `projects` are replaced wholesale, so deleted sessions
    /// fall out; entries from any other claude dir (`--claude-dir`) are kept,
    /// so switching between trees does not throw the other one's work away.
    fn store(mut self, sessions: &[SessionMeta], projects: &Path) {
        let Some(path) = Self::file() else { return };
        self.entries.retain(|_, s| !s.path.starts_with(projects));
        self.entries.extend(
            sessions
                .iter()
                .map(|s| (s.path.to_string_lossy().into_owned(), s.clone())),
        );
        let cache = Cache {
            schema: CACHE_SCHEMA,
            entries: self.entries,
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

    #[test]
    fn projects_summarise_in_session_order() {
        let meta = |slug: &str, cwd: Option<&str>, bytes: u64| SessionMeta {
            id: String::new(),
            path: PathBuf::new(),
            project_slug: slug.into(),
            size_bytes: bytes,
            modified_ms: 0,
            first_ts: None,
            last_ts: None,
            title: String::new(),
            cwd: cwd.map(str::to_string),
            git_branch: None,
            user_msgs: 0,
            assistant_msgs: 0,
            tool_calls: 0,
        };
        // Newest first, as `build` leaves them.
        let index = Index {
            sessions: vec![
                meta("b", None, 1),
                meta("a", Some("/src/a"), 10),
                meta("b", Some("/src/b"), 2),
                meta("a", Some("/elsewhere"), 100),
            ],
            warnings: Vec::new(),
        };
        let p = index.projects();
        let rows: Vec<(&str, &str, usize, u64)> = p
            .iter()
            .map(|p| (p.slug.as_str(), p.label.as_str(), p.count, p.bytes))
            .collect();
        // Label is the first recorded cwd; a slug with none shows as itself.
        assert_eq!(rows, [("b", "/src/b", 2, 3), ("a", "/src/a", 2, 110)]);
        let none = Index {
            sessions: vec![meta("c", None, 0)],
            warnings: Vec::new(),
        };
        assert_eq!(none.projects()[0].label, "c");
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
    fn meta_only_sessions_are_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("abc.jsonl");
        std::fs::write(
            &path,
            br#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>Caveat</local-command-caveat>"}}"#,
        )
        .unwrap();
        let meta = scan_file("slug", &path, 1, 0).unwrap();
        assert_eq!(meta.user_msgs, 0);
        assert_eq!(meta.title, "(empty session)");
    }

    #[test]
    fn truncate_counts_chars() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
        assert_eq!(truncate("日本語テ", 3), "日本…");
        assert_eq!(truncate("ab  cd", 4), "ab…");
        assert_eq!(truncate("", 0), "");
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
