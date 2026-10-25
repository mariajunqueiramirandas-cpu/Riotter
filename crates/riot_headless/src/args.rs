//! A tiny `--key value` argument parser that reports unused arguments.

use std::cell::RefCell;
use std::collections::BTreeSet;

pub struct Args {
    command: Option<String>,
    pairs: Vec<(String, String)>,
    used: RefCell<BTreeSet<String>>,
}

impl Args {
    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1))
    }

    pub fn parse(it: impl Iterator<Item = String>) -> Self {
        let mut command = None;
        let mut pairs = Vec::new();
        let mut it = it.peekable();
        while let Some(a) = it.next() {
            if let Some(key) = a.strip_prefix("--") {
                let value = match it.peek() {
                    Some(v) if !v.starts_with("--") => it.next().unwrap(),
                    _ => "true".to_string(),
                };
                pairs.push((key.to_string(), value));
            } else if command.is_none() {
                command = Some(a);
            } else {
                pairs.push((a, String::new()));
            }
        }
        Args {
            command,
            pairs,
            used: RefCell::new(BTreeSet::new()),
        }
    }

    pub fn command(&mut self) -> Option<String> {
        self.command.clone()
    }

    pub fn str(&self, key: &str) -> Option<String> {
        self.used.borrow_mut().insert(key.to_string());
        self.pairs
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    pub fn flag(&self, key: &str) -> bool {
        self.str(key).is_some_and(|v| v != "false")
    }

    pub fn u32(&self, key: &str) -> Option<u32> {
        self.str(key).and_then(|v| v.replace('_', "").parse().ok())
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.str(key).and_then(|v| v.replace('_', "").parse().ok())
    }

    pub fn usize(&self, key: &str) -> Option<usize> {
        self.str(key).and_then(|v| v.replace('_', "").parse().ok())
    }

    pub fn f32(&self, key: &str) -> Option<f32> {
        self.str(key).and_then(|v| v.parse().ok())
    }

    /// Arguments nobody asked for.
    pub fn unused(&self) -> Option<String> {
        let used = self.used.borrow();
        let rest: Vec<String> = self
            .pairs
            .iter()
            .filter(|(k, _)| !used.contains(k))
            .map(|(k, v)| format!("--{k} {v}"))
            .collect();
        (!rest.is_empty()).then(|| rest.join(" "))
    }
}
