//! Pi sessions: `<agent-dir>/sessions/<encoded-cwd>/*.jsonl`, where the
//! default agent directory is `~/.pi/agent`. A session starts with a `session`
//! header and appends `message` records with OpenAI-style content blocks.

use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::{canonical, file_mtime_ms, home_dir, SessionRef, SessionStore, Turn};
use crate::harness::Invocation;
use crate::json::Json;

pub(crate) struct PiSessions;

impl SessionStore for PiSessions {
    fn harness(&self) -> &'static str {
        "pi"
    }

    fn list(&self, cwd: &Path) -> Result<Vec<SessionRef>, String> {
        let dir = match project_session_dir(cwd) {
            Some(dir) if dir.is_dir() => dir,
            _ => return Ok(Vec::new()),
        };

        let mut sessions = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(_) => continue,
            };
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(session) = read_session(&path, cwd) {
                sessions.push(session);
            }
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_ms));
        Ok(sessions)
    }

    fn resume_invocation(&self, id: &str, _cwd: &Path, _yolo: bool) -> Result<Invocation, String> {
        let args = if id.is_empty() {
            vec!["--continue".to_string()]
        } else {
            vec!["--session".to_string(), id.to_string()]
        };
        Ok(Invocation::new("pi", args))
    }

    fn transcript(&self, id: &str, cwd: &Path) -> Result<Vec<Turn>, String> {
        let dir = project_session_dir(cwd).ok_or_else(|| {
            format!(
                "cannot resolve the pi session directory for {}",
                cwd.display()
            )
        })?;
        let path = find_session(&dir, id)
            .ok_or_else(|| format!("pi session {id} not found for {}", cwd.display()))?;
        let file = File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;

        let mut turns = Vec::new();
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let json = match Json::parse(line.trim()) {
                Ok(json) => json,
                Err(_) => continue,
            };
            if json.get("type").and_then(Json::as_str) != Some("message") {
                continue;
            }
            let message = match json.get("message") {
                Some(message) => message,
                None => continue,
            };
            let role = match message.get("role").and_then(Json::as_str) {
                Some(role @ ("user" | "assistant")) => role,
                _ => continue,
            };
            if let Some(text) = message_text(message) {
                let text = text.trim();
                if !text.is_empty() {
                    turns.push(Turn {
                        role: role.to_string(),
                        text: text.to_string(),
                    });
                }
            }
        }
        Ok(turns)
    }
}

/// Compute Pi's cwd directory name: `/work/my-app` becomes
/// `--work-my-app--`. Pi uses this layout unless a custom session directory is
/// supplied, in which case all `.jsonl` sessions live directly in that folder.
fn project_session_dir(cwd: &Path) -> Option<PathBuf> {
    if let Some(dir) = env_path("PI_CODING_AGENT_SESSION_DIR") {
        return Some(dir);
    }
    let agent_dir = env_path("PI_CODING_AGENT_DIR")
        .or_else(|| home_dir().map(|home| home.join(".pi").join("agent")))?;
    Some(agent_dir.join("sessions").join(slug_for_cwd(cwd)))
}

fn env_path(name: &str) -> Option<PathBuf> {
    let value = env::var_os(name)?;
    let path = PathBuf::from(value);
    if path == Path::new("~") {
        return home_dir();
    }
    if let Ok(rest) = path.strip_prefix("~") {
        return home_dir().map(|home| home.join(rest));
    }
    Some(path)
}

fn slug_for_cwd(cwd: &Path) -> String {
    let path = canonical(cwd);
    let value = path.to_string_lossy();
    let trimmed = value.trim_start_matches(['/', '\\']);
    let safe: String = trimmed
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    format!("--{safe}--")
}

fn find_session(dir: &Path, id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let header = match read_header(&path) {
            Some(header) => header,
            None => continue,
        };
        if header.0 == id {
            return Some(path);
        }
    }
    None
}

fn read_session(path: &Path, cwd: &Path) -> Option<SessionRef> {
    let (id, session_cwd) = read_header(path)?;
    if !cwd_matches(&session_cwd, cwd) {
        return None;
    }
    let (title, message_count) = summarize(path);
    Some(SessionRef {
        harness: "pi".to_string(),
        id,
        cwd: session_cwd,
        updated_ms: file_mtime_ms(path),
        title,
        message_count,
        delegated: false,
    })
}

fn read_header(path: &Path) -> Option<(String, String)> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let json = Json::parse(first.trim()).ok()?;
    if json.get("type").and_then(Json::as_str) != Some("session") {
        return None;
    }
    let id = json.get("id").and_then(Json::as_str)?.to_string();
    let cwd = json.get("cwd").and_then(Json::as_str)?.to_string();
    Some((id, cwd))
}

fn cwd_matches(session_cwd: &str, cwd: &Path) -> bool {
    session_cwd == cwd.to_string_lossy() || canonical(Path::new(session_cwd)) == canonical(cwd)
}

fn summarize(path: &Path) -> (String, Option<usize>) {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return ("(unreadable)".to_string(), None),
    };
    let mut title: Option<String> = None;
    let mut first_user: Option<String> = None;
    let mut messages = 0usize;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let json = match Json::parse(line.trim()) {
            Ok(json) => json,
            Err(_) => continue,
        };
        match json.get("type").and_then(Json::as_str) {
            Some("session_info") => {
                title = json
                    .get("name")
                    .and_then(Json::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_string);
            }
            Some("message") => {
                messages += 1;
                let message = match json.get("message") {
                    Some(message) => message,
                    None => continue,
                };
                if first_user.is_none()
                    && message.get("role").and_then(Json::as_str) == Some("user")
                {
                    first_user = message_text(message).filter(|text| !text.trim().is_empty());
                }
            }
            _ => {}
        }
    }

    (
        title
            .or(first_user)
            .unwrap_or_else(|| "(pi session)".to_string()),
        (messages > 0).then_some(messages),
    )
}

fn message_text(message: &Json) -> Option<String> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let mut parts = Vec::new();
    for block in content.as_array()? {
        if block.get("type").and_then(Json::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Json::as_str) {
                parts.push(text.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_default_pi_session_directory() {
        assert_eq!(
            slug_for_cwd(Path::new("/home/kerry/Work/my-app")),
            "--home-kerry-Work-my-app--"
        );
    }

    #[test]
    fn extracts_only_text_blocks() {
        let message = Json::parse(
            r#"{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"answer"}]}"#,
        )
        .unwrap();
        assert_eq!(message_text(&message), Some("answer".to_string()));
    }
}
