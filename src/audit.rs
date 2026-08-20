//! Append-only audit log of tool activity.
//!
//! Security research on MCP is emphatic about recording end-to-end tool
//! activity: what was called, with which arguments, what the guard decided, and
//! how it turned out. We write one JSON object per line (JSONL) so the log is
//! easy to grep, tail, or load into another tool. Logging is best-effort and
//! never fails a request.

use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// One audited tool call.
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    /// Unix time (seconds) the call resolved.
    pub ts: u64,
    /// Tool name.
    pub tool: String,
    /// Guard decision: allow | blocked | confirmed | declined | suppressed | local | unknown.
    pub decision: String,
    /// Outcome: ok | error | skipped.
    pub status: String,
    /// The (normalized) arguments the tool was called with.
    pub args: Value,
    /// Optional extra detail (error text, block reason, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEntry {
    pub fn new(tool: &str, decision: &str, status: &str, args: Value) -> Self {
        AuditEntry {
            ts: now_secs(),
            tool: tool.to_string(),
            decision: decision.to_string(),
            status: status.to_string(),
            args,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let d = detail.into();
        if !d.is_empty() {
            self.detail = Some(d);
        }
        self
    }
}

/// A logger that appends entries to a file, or does nothing when disabled.
#[derive(Clone, Default)]
pub struct AuditLogger {
    path: Option<PathBuf>,
}

impl AuditLogger {
    /// `None` disables logging.
    pub fn new(path: Option<String>) -> Self {
        AuditLogger {
            path: path.map(PathBuf::from),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Append one entry. Best-effort: on any IO error we warn to stderr and
    /// carry on, since auditing must never break the agent.
    pub fn record(&self, entry: &AuditEntry) {
        let Some(path) = &self.path else { return };
        let line = format_line(entry);
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = result {
            eprintln!("audit: could not write to {}: {e}", path.display());
        }
    }
}

/// Serialize an entry as a single JSONL line (trailing newline included).
pub fn format_line(entry: &AuditEntry) -> String {
    let mut s = serde_json::to_string(entry).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn line_is_valid_jsonl() {
        let e = AuditEntry::new("execute_sql", "confirmed", "ok", json!({"sql": "SELECT 1"}));
        let line = format_line(&e);
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["tool"], "execute_sql");
        assert_eq!(parsed["decision"], "confirmed");
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["args"]["sql"], "SELECT 1");
        assert!(parsed["ts"].as_u64().unwrap() > 0);
    }

    #[test]
    fn detail_is_omitted_when_empty() {
        let e = AuditEntry::new("t", "blocked", "skipped", json!({})).with_detail("");
        let parsed: Value = serde_json::from_str(format_line(&e).trim()).unwrap();
        assert!(parsed.get("detail").is_none());
    }

    #[test]
    fn disabled_logger_is_noop() {
        let logger = AuditLogger::new(None);
        assert!(!logger.is_enabled());
        // Must not panic or write anything.
        logger.record(&AuditEntry::new("t", "allow", "ok", json!({})));
    }

    #[test]
    fn enabled_logger_appends_lines() {
        let mut path = std::env::temp_dir();
        path.push(format!("pgmcp_audit_test_{}.jsonl", now_secs_nanos()));
        let logger = AuditLogger::new(Some(path.to_string_lossy().to_string()));
        logger.record(&AuditEntry::new("a", "allow", "ok", json!({})));
        logger.record(&AuditEntry::new("b", "blocked", "skipped", json!({})));
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(lines[0]).unwrap()["tool"],
            "a"
        );
        let _ = std::fs::remove_file(&path);
    }

    fn now_secs_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }
}
