//! Tunables files (DESIGN §19 iteration loop). `data/tunables.ron` is a RON
//! map of tunable name → number:
//!
//! ```ron
//! {
//!     // Ticks between an agent's rethinks (K).
//!     "think_interval": 10.0,
//! }
//! ```
//!
//! A file may list only some tunables; the rest keep their defaults. Unknown
//! names are an error. `riot_app` reloads the file while a riot runs and
//! turns the changes into `SetTunable` commands with [`tunable_commands`].

use std::fmt;
use std::path::Path;

use riot_sim::{Command, CommandKind, TUNABLES, Tunables};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use riot_data_core::{DataError, Result, line_of};

/// Parses a tunables file into `(name, value)` pairs, in file order. Every
/// name must be a tunable, at most once.
pub fn parse_tunables(text: &str) -> Result<Vec<(String, f32)>> {
    let Entries(entries) = ron::Options::default()
        .from_str::<Entries>(text)
        .map_err(ron_error)?;
    let mut out: Vec<(String, f32)> = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let at = || {
            line_of(text, &format!("\"{name}\""))
                .or_else(|| line_of(text, &name))
                .map_or(String::new(), |l| format!("line {l}: "))
        };
        if Tunables::id_of(&name).is_none() {
            let hint = closest_tunable(&name)
                .map(|n| format!("; did you mean `{n}`?"))
                .unwrap_or_default();
            return Err(DataError::new(format!(
                "{}unknown tunable `{name}`{hint}",
                at()
            )));
        }
        if out.iter().any(|(n, _)| *n == name) {
            return Err(DataError::new(format!("{}`{name}` is listed twice", at())));
        }
        let v = value as f32;
        if !v.is_finite() {
            return Err(DataError::new(format!(
                "{}`{name}` must be a finite number",
                at()
            )));
        }
        out.push((name, v));
    }
    Ok(out)
}

/// Loads a tunables file: the defaults, overridden by what the file lists.
pub fn load_tunables(path: &Path) -> Result<Tunables> {
    let text = std::fs::read_to_string(path).map_err(|e| DataError::io(path, e))?;
    let values = parse_tunables(&text).map_err(|e| e.in_file(path))?;
    let mut t = Tunables::default();
    apply(&mut t, &values);
    Ok(t)
}

/// Sets each named tunable (names already checked by [`parse_tunables`]).
pub fn apply(t: &mut Tunables, values: &[(String, f32)]) {
    for (name, v) in values {
        t.set_by_name(name, *v);
    }
}

/// One `SetTunable` command at `tick` for every tunable whose value differs
/// between `old` and `new` (hot reload: DESIGN §12 rule 9).
pub fn tunable_commands(old: &Tunables, new: &Tunables, tick: u32) -> Vec<Command> {
    (0..TUNABLES.len() as u16)
        .filter_map(|id| {
            let (a, b) = (old.get(id)?, new.get(id)?);
            (a.to_bits() != b.to_bits())
                .then(|| Command::at(tick, CommandKind::SetTunable { id, value: b }))
        })
        .collect()
}

/// The contents of `data/tunables.ron`: every tunable with its default and
/// its doc string as a comment.
pub fn default_tunables_ron() -> String {
    let defaults = Tunables::default();
    let mut s = String::from(
        "// Riotter tunables (DESIGN §19). Every tunable with its default value.\n\
         // riot_app reloads this file while a riot runs and applies changed values\n\
         // as SetTunable commands on the next tick. A scenario can name another\n\
         // file that lists only some tunables. Unknown names are an error.\n\
         {\n",
    );
    for (id, (name, doc)) in TUNABLES.iter().enumerate() {
        let v = defaults.get(id as u16).unwrap_or_default();
        s.push_str(&format!(
            "    // {doc}\n    \"{name}\": {},\n",
            format_value(v)
        ));
    }
    s.push_str("}\n");
    s
}

/// Shortest text that reads back as `v`, always with a decimal point.
fn format_value(v: f32) -> String {
    let s = v.to_string();
    if s.contains('.') { s } else { s + ".0" }
}

/// The tunable name nearest to `name` by edit distance, if any is close.
fn closest_tunable(name: &str) -> Option<&'static str> {
    TUNABLES
        .iter()
        .map(|(n, _)| (edit_distance(n, name), *n))
        .filter(|&(d, n)| d <= 3.max(n.len() / 4))
        .min()
        .map(|(_, n)| n)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Turns a RON error into a [`DataError`] with its line and column (for
/// every RON file: tunables here, scenarios in `riot_data_scenario`).
pub fn ron_error(e: ron::error::SpannedError) -> DataError {
    let p = e.span.start;
    DataError::caused_by(format!("line {}, column {}: {}", p.line, p.col, e.code), e)
}

/// Map entries in file order (a `BTreeMap` would sort them and lose
/// duplicates).
struct Entries(Vec<(String, f64)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Entries;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of tunable names to numbers: { \"name\": 1.0, ... }")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut m: A,
            ) -> std::result::Result<Entries, A::Error> {
                let mut v = Vec::new();
                while let Some((k, Number(x))) = m.next_entry::<String, Number>()? {
                    v.push((k, x));
                }
                Ok(Entries(v))
            }
        }
        d.deserialize_map(V)
    }
}

/// Any RON number, integer or float.
struct Number(f64);

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Number;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Number, E> {
                Ok(Number(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Number, E> {
                Ok(Number(v as f64))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Number, E> {
                Ok(Number(v as f64))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_in_order_and_checks_names() {
        let v = parse_tunables("{ \"hop_loss\": 4, \"think_interval\": 8.5 }").unwrap();
        assert_eq!(
            v,
            vec![("hop_loss".into(), 4.0), ("think_interval".into(), 8.5)]
        );
        let e = parse_tunables("{\n  \"think_intervl\": 8.0,\n}")
            .unwrap_err()
            .to_string();
        assert!(e.contains("line 2: unknown tunable `think_intervl`"), "{e}");
        assert!(e.contains("did you mean `think_interval`"), "{e}");
        let e = parse_tunables("{ \"hop_loss\": 1, \"hop_loss\": 2 }").unwrap_err();
        assert!(e.to_string().contains("listed twice"), "{e}");
        let e = parse_tunables("{ \"hop_loss\": \"x\" }").unwrap_err();
        assert!(e.to_string().contains("line 1"), "{e}");
    }

    #[test]
    fn default_file_round_trips() {
        let v = parse_tunables(&default_tunables_ron()).unwrap();
        assert_eq!(v.len(), TUNABLES.len());
        let mut t = Tunables {
            think_interval: 3.0,
            ..Default::default()
        };
        apply(&mut t, &v);
        assert_eq!(t, Tunables::default());
    }

    #[test]
    fn commands_for_changes_only() {
        let old = Tunables::default();
        let mut new = old;
        new.hop_loss = 9.0;
        new.camera_radius = 30.0;
        let cmds = tunable_commands(&old, &new, 77);
        let ids = [
            Tunables::id_of("hop_loss").unwrap(),
            Tunables::id_of("camera_radius").unwrap(),
        ];
        assert_eq!(
            cmds,
            vec![
                Command::at(
                    77,
                    CommandKind::SetTunable {
                        id: ids[0],
                        value: 9.0
                    }
                ),
                Command::at(
                    77,
                    CommandKind::SetTunable {
                        id: ids[1],
                        value: 30.0
                    }
                ),
            ]
        );
        assert!(tunable_commands(&old, &old, 0).is_empty());
    }
}
