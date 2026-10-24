//! Tunables hot reload (DESIGN §19): `--tunables path` is polled once a
//! second; changed values become `SetTunable` commands, so a change is
//! applied at a specific tick and recorded like every other input.
//!
//! The file is a RON map `{ "name": value, ... }` (bare names, a struct
//! form `Tunables( name: value )`, comments and trailing commas also work).
//! This small parser stands in until `riot_data` provides its own.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use riot_sim::{CommandKind, Tunables};

/// Parse a RON map of names to numbers.
pub fn parse_ron_map(text: &str) -> Result<Vec<(String, f32)>, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    p.skip();
    // Optional struct name: `Tunables( ... )`.
    if p.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
        p.ident()?;
        p.skip();
    }
    let close = match p.next() {
        Some(b'{') => b'}',
        Some(b'(') => b')',
        _ => return Err(p.err("expected `{` or `(`")),
    };
    let mut out = Vec::new();
    loop {
        p.skip();
        if p.peek() == Some(close) {
            p.i += 1;
            break;
        }
        let key = match p.peek() {
            Some(b'"') => p.string()?,
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => p.ident()?,
            _ => return Err(p.err("expected a tunable name")),
        };
        p.skip();
        if p.next() != Some(b':') {
            return Err(p.err("expected `:`"));
        }
        p.skip();
        let v = p.number()?;
        out.push((key, v));
        p.skip();
        match p.peek() {
            Some(b',') => p.i += 1,
            Some(c) if c == close => {}
            _ => return Err(p.err("expected `,` or the closing bracket")),
        }
    }
    p.skip();
    if p.i < p.s.len() {
        return Err(p.err("unexpected text after the map"));
    }
    Ok(out)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek();
        self.i += c.is_some() as usize;
        c
    }

    fn err(&self, what: &str) -> String {
        let line = 1 + self.s[..self.i.min(self.s.len())]
            .iter()
            .filter(|&&c| c == b'\n')
            .count();
        format!("line {line}: {what}")
    }

    /// Skip whitespace and comments.
    fn skip(&mut self) {
        loop {
            match (self.peek(), self.s.get(self.i + 1)) {
                (Some(c), _) if c.is_ascii_whitespace() => self.i += 1,
                (Some(b'/'), Some(b'/')) => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.i += 2;
                    while self.i < self.s.len() && !self.s[self.i..].starts_with(b"*/") {
                        self.i += 1;
                    }
                    self.i = (self.i + 2).min(self.s.len());
                }
                _ => return,
            }
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.i += 1;
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let start = self.i;
        while self.peek().is_some_and(|c| c != b'"' && c != b'\n') {
            self.i += 1;
        }
        if self.peek() != Some(b'"') {
            return Err(self.err("unterminated string"));
        }
        let s = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        self.i += 1;
        Ok(s)
    }

    fn number(&mut self) -> Result<f32, String> {
        let start = self.i;
        while self.peek().is_some_and(|c| {
            c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E' | b'_')
        }) {
            self.i += 1;
        }
        let t: String = String::from_utf8_lossy(&self.s[start..self.i]).replace('_', "");
        t.parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| self.err(&format!("`{t}` is not a number")))
    }
}

/// Read and parse a tunables file.
pub fn read_tunables(path: &Path) -> Result<Vec<(String, f32)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_ron_map(&text)
}

/// Apply parsed values to `tun`; returns the names that are not tunables.
pub fn apply(values: &[(String, f32)], tun: &mut Tunables) -> Vec<String> {
    let mut unknown = Vec::new();
    for (name, v) in values {
        if !tun.set_by_name(name, *v) {
            unknown.push(name.clone());
        }
    }
    unknown
}

/// Polls a tunables file and turns edits into commands.
pub struct TunablesWatcher {
    path: PathBuf,
    mtime: Option<SystemTime>,
    next_poll: Instant,
    /// The values the simulation has (as far as this watcher knows).
    current: Tunables,
    /// What the last reload did, for the HUD.
    pub status: Option<String>,
}

impl TunablesWatcher {
    /// Watch `path`; `current` is what the world started with.
    pub fn new(path: PathBuf, current: Tunables) -> TunablesWatcher {
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        TunablesWatcher {
            path,
            mtime,
            next_poll: Instant::now() + Duration::from_secs(1),
            current,
            status: None,
        }
    }

    /// At most once a second: if the file changed, the `SetTunable`
    /// commands for every value that differs from what the sim has.
    pub fn poll(&mut self, now: Instant) -> Vec<CommandKind> {
        if now < self.next_poll {
            return Vec::new();
        }
        self.next_poll = now + Duration::from_secs(1);
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime.is_none() || mtime == self.mtime {
            return Vec::new();
        }
        self.mtime = mtime;
        let values = match read_tunables(&self.path) {
            Ok(v) => v,
            Err(e) => {
                self.status = Some(format!("tunables: {e}"));
                eprintln!("{}: {e}", self.path.display());
                return Vec::new();
            }
        };
        self.changes(&values)
    }

    /// Commands for the values in `values` that differ from `current`.
    fn changes(&mut self, values: &[(String, f32)]) -> Vec<CommandKind> {
        let mut out = Vec::new();
        let mut unknown = Vec::new();
        for (name, value) in values {
            let Some(id) = Tunables::id_of(name) else {
                unknown.push(name.as_str());
                continue;
            };
            if self.current.get(id) != Some(*value) {
                self.current.set(id, *value);
                out.push(CommandKind::SetTunable { id, value: *value });
            }
        }
        let mut msg = format!("tunables reloaded: {} changed", out.len());
        if !unknown.is_empty() {
            msg += &format!(", unknown: {}", unknown.join(", "));
        }
        eprintln!("{msg}");
        self.status = Some(msg);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_maps_structs_comments_and_trailing_commas() {
        let text = r#"
            // contact tuning
            {
                "contact_iters": 3,
                gas_decay: 0.99, /* slower */
                "throw_range": -1.5e1,
            }
        "#;
        let v = parse_ron_map(text).unwrap();
        assert_eq!(
            v,
            vec![
                ("contact_iters".to_string(), 3.0),
                ("gas_decay".to_string(), 0.99),
                ("throw_range".to_string(), -15.0),
            ]
        );
        let s = parse_ron_map("Tunables(think_interval: 12)").unwrap();
        assert_eq!(s, vec![("think_interval".to_string(), 12.0)]);
        assert_eq!(parse_ron_map("{}").unwrap(), vec![]);
    }

    #[test]
    fn reports_errors_with_lines() {
        let e = parse_ron_map("{\n \"a\": 1,\n \"b\" 2 }").unwrap_err();
        assert!(e.starts_with("line 3"), "{e}");
        assert!(parse_ron_map("{ \"a\": x }").is_err());
        assert!(parse_ron_map("{ \"a\": 1 } trailing").is_err());
        assert!(parse_ron_map("[1, 2]").is_err());
    }

    #[test]
    fn only_changed_values_become_commands() {
        let mut w = TunablesWatcher::new(PathBuf::from("/nonexistent"), Tunables::default());
        let id = Tunables::id_of("gas_decay").unwrap();
        let default = Tunables::default().gas_decay;
        let cmds = w.changes(&[
            ("gas_decay".into(), 0.9),
            ("think_interval".into(), Tunables::default().think_interval),
            ("nope".into(), 1.0),
        ]);
        assert_eq!(cmds, vec![CommandKind::SetTunable { id, value: 0.9 }]);
        assert!(w.status.as_deref().unwrap().contains("unknown: nope"));
        assert!(w.changes(&[("gas_decay".into(), 0.9)]).is_empty());
        assert_ne!(default, 0.9);
    }
}
