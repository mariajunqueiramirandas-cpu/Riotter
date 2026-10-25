//! Tunables hot reload (DESIGN §19): `--tunables path` is polled once a
//! second; changed values become `SetTunable` commands, so a change is
//! applied at a specific tick and recorded like every other input.
//!
//! The file is `data/tunables.ron`'s format, `{ "name": value, ... }`,
//! parsed by `riot_data`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use riot_sim::{CommandKind, Tunables};

/// Read and parse a tunables file.
pub fn read_tunables(path: &Path) -> Result<Vec<(String, f32)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    riot_data::parse_tunables(&text).map_err(|e| e.to_string())
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
