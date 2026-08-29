use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::index::truncate;

#[derive(Debug, Clone)]
pub enum Event {
    User(String),
    Assistant(String),
    Thinking(String),
    ToolUse {
        name: String,
        headline: String,
        raw: String,
    },
    ToolResult {
        is_error: bool,
        preview: String,
        raw: String,
    },
    Meta(String),
}

impl Event {
    /// Text used for in-session search.
    pub fn searchable(&self) -> &str {
        match self {
            Event::User(t) | Event::Assistant(t) | Event::Thinking(t) | Event::Meta(t) => t,
            Event::ToolUse { headline, .. } => headline,
            Event::ToolResult { preview, .. } => preview,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub ts: Option<DateTime<Utc>>,
    pub sidechain: bool,
    pub event: Event,
}

#[derive(Debug, Clone)]
pub struct LoadOpts {
    pub max_entries: usize,
    pub include_sidechains: bool,
    pub include_meta: bool,
}

impl Default for LoadOpts {
    fn default() -> Self {
        Self {
            max_entries: 2000,
            include_sidechains: false,
            include_meta: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    /// True when `max_entries` cut the load short.
    pub truncated: bool,
}

pub fn load(path: &Path, opts: &LoadOpts) -> Result<Transcript> {
    let reader = BufReader::with_capacity(256 * 1024, File::open(path)?);
    let mut out = Transcript::default();

    for line in reader.split(b'\n') {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };

        let sidechain = v["isSidechain"].as_bool().unwrap_or(false);
        if sidechain && !opts.include_sidechains {
            continue;
        }
        let ts = v["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));

        let events = events_from(&v)
            .into_iter()
            .filter(|e| opts.include_meta || !matches!(e, Event::Meta(_)));
        for event in events {
            // Checked per event, not per line: a trailing line that yields
            // nothing must not report the transcript as cut short.
            if out.entries.len() >= opts.max_entries {
                out.truncated = true;
                return Ok(out);
            }
            out.entries.push(Entry {
                ts,
                sidechain,
                event,
            });
        }
    }
    Ok(out)
}

/// Meta events are produced unconditionally; `load` drops them when they are
/// not wanted, so every source of them is filtered in one place.
fn events_from(v: &Value) -> Vec<Event> {
    match v["type"].as_str().unwrap_or("") {
        "user" => blocks_of(&v["message"]["content"], false),
        "assistant" => blocks_of(&v["message"]["content"], true),
        "system" => {
            let subtype = v["subtype"].as_str().unwrap_or("system");
            let body = v["content"].as_str().unwrap_or("");
            vec![Event::Meta(format!(
                "system/{subtype} {}",
                truncate(body, 200)
            ))]
        }
        "attachment" => {
            let kind = v["attachment"]["type"].as_str().unwrap_or("attachment");
            vec![Event::Meta(format!("attachment: {kind}"))]
        }
        "" => Vec::new(),
        other => vec![Event::Meta(other.to_string())],
    }
}

fn blocks_of(content: &Value, assistant: bool) -> Vec<Event> {
    // Plain-string content is always a user turn.
    if let Some(s) = content.as_str() {
        return user_text(s.trim()).into_iter().collect();
    }

    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for b in blocks {
        match b["type"].as_str().unwrap_or("") {
            "text" => {
                let Some(t) = b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) else {
                    continue;
                };
                if assistant {
                    out.push(Event::Assistant(t.to_string()));
                } else if let Some(event) = user_text(t) {
                    out.push(event);
                }
            }
            "thinking" => {
                if let Some(t) = b["thinking"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    out.push(Event::Thinking(t.to_string()));
                }
            }
            "tool_use" => {
                let name = b["name"].as_str().unwrap_or("tool").to_string();
                out.push(Event::ToolUse {
                    headline: headline_for(&b["input"]),
                    raw: pretty(&b["input"]),
                    name,
                });
            }
            "tool_result" => {
                let body = result_text(&b["content"]);
                out.push(Event::ToolResult {
                    is_error: b["is_error"].as_bool().unwrap_or(false),
                    preview: truncate(&body.replace('\n', " ⏎ "), 300),
                    raw: body,
                });
            }
            _ => {}
        }
    }
    out
}

/// Every session opens with CLI-generated wrappers - a slash command, a caveat
/// about local commands, injected reminders. Rendering those as user turns
/// buries the real prompt, so collapse them to one meta line (or drop them).
fn user_text(text: &str) -> Option<Event> {
    if let Some(name) = crate::index::tag_value(text, "command-name") {
        let args = crate::index::tag_value(text, "command-args").unwrap_or_default();
        return Some(Event::Meta(truncate(format!("{name} {args}").trim(), 200)));
    }
    let stripped = crate::index::strip_noise_tags(text);
    let stripped = stripped.trim();
    (!stripped.is_empty()).then(|| Event::User(stripped.to_string()))
}

/// The one field that says what a tool call actually does.
fn headline_for(input: &Value) -> String {
    const SALIENT: [&str; 8] = [
        "command",
        "file_path",
        "pattern",
        "prompt",
        "path",
        "url",
        "query",
        "description",
    ];
    for key in SALIENT {
        if let Some(s) = input[key].as_str() {
            let s = s.trim();
            if !s.is_empty() {
                return truncate(&s.replace('\n', " ⏎ "), 160);
            }
        }
    }
    truncate(&input.to_string(), 160)
}

fn result_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(blocks) = content.as_array() {
        return blocks
            .iter()
            .map(|b| match b["type"].as_str().unwrap_or("") {
                "text" => b["text"].as_str().unwrap_or("").to_string(),
                other => format!("[{other}]"),
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if content.is_null() {
        return String::new();
    }
    content.to_string()
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn role(e: &Event) -> &'static str {
        match e {
            Event::User(_) => "user",
            Event::Assistant(_) => "assistant",
            Event::Thinking(_) => "thinking",
            Event::ToolUse { .. } => "tool",
            Event::ToolResult { .. } => "result",
            Event::Meta(_) => "meta",
        }
    }

    fn everything() -> LoadOpts {
        LoadOpts {
            max_entries: usize::MAX,
            include_sidechains: true,
            include_meta: true,
        }
    }

    fn fixture(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_the_real_record_shapes() {
        let f = fixture(&[
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"user","timestamp":"2026-08-23T18:48:01.103Z","message":{"role":"user","content":"do the thing"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"ok"},{"type":"tool_use","name":"Bash","input":{"command":"git status","description":"x"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"on branch main","is_error":false}]}}"#,
            "not json at all",
        ]);

        let t = load(f.path(), &LoadOpts::default()).unwrap();
        let roles: Vec<&str> = t.entries.iter().map(|e| role(&e.event)).collect();
        assert_eq!(
            roles,
            ["user", "thinking", "assistant", "tool", "result"],
            "meta records excluded by default; bad lines skipped"
        );

        match &t.entries[3].event {
            Event::ToolUse { name, headline, .. } => {
                assert_eq!(name, "Bash");
                assert_eq!(headline, "git status", "command beats description");
            }
            other => panic!("{other:?}"),
        }
        assert!(t.entries[0].ts.is_some());
        assert!(!t.truncated);
    }

    #[test]
    fn cli_wrappers_collapse_instead_of_burying_the_prompt() {
        let f = fixture(&[
            r#"{"type":"user","message":{"content":"<local-command-caveat>Caveat: boilerplate.</local-command-caveat>"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name><command-message>clear</command-message><command-args></command-args>"}}"#,
            r#"{"type":"user","message":{"content":"<system-reminder>noise</system-reminder>the real prompt"}}"#,
        ]);

        // Meta is off by default, so only the real prompt survives...
        let t = load(f.path(), &LoadOpts::default()).unwrap();
        assert_eq!(t.entries.len(), 1);
        assert!(matches!(&t.entries[0].event, Event::User(s) if s == "the real prompt"));

        // ...and the command shows as one meta line when meta is on.
        let t = load(f.path(), &everything()).unwrap();
        let roles: Vec<&str> = t.entries.iter().map(|e| role(&e.event)).collect();
        assert_eq!(roles, ["meta", "user"], "caveat dropped, command collapsed");
        assert!(matches!(&t.entries[0].event, Event::Meta(s) if s == "/clear"));
    }

    #[test]
    fn sidechains_and_meta_are_opt_in() {
        let f = fixture(&[
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"hidden"}}"#,
        ]);
        assert!(load(f.path(), &LoadOpts::default())
            .unwrap()
            .entries
            .is_empty());
        assert_eq!(load(f.path(), &everything()).unwrap().entries.len(), 2);
    }

    #[test]
    fn max_entries_truncates() {
        let line = r#"{"type":"user","message":{"content":"hi"}}"#;
        let f = fixture(&[line, line, line]);
        let opts = LoadOpts {
            max_entries: 2,
            ..Default::default()
        };
        let t = load(f.path(), &opts).unwrap();
        assert_eq!(t.entries.len(), 2);
        assert!(t.truncated);
    }

    #[test]
    fn headline_falls_back_to_json() {
        let h = headline_for(&serde_json::json!({"todos": [1, 2]}));
        assert_eq!(h, r#"{"todos":[1,2]}"#);
    }

    #[test]
    fn tool_result_block_arrays_flatten() {
        let body = result_text(&serde_json::json!([
            {"type": "text", "text": "line"},
            {"type": "image"},
        ]));
        assert_eq!(body, "line\n[image]");
        assert_eq!(result_text(&Value::Null), "");
    }
}
