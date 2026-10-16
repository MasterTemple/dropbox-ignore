//! Working out what `check` needs to change, separately from applying it, so the
//! CLI and the TUI can both preview a plan before touching anything.

use std::path::{Path, PathBuf};

use crate::{
    dropbox,
    rules::{Rule, Rules},
    ui::{self, Mark},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A rule matches but the item is still synced
    Ignore,
    /// Ignored without a rule, and the caller asked to sync such items again
    Unignore,
    /// Ignored without a rule; reported but left alone
    Unmatched,
}

impl Action {
    pub fn mark(self) -> Mark {
        match self {
            Action::Ignore => Mark::Ignored,
            Action::Unignore => Mark::Restored,
            Action::Unmatched => Mark::IgnoredManual,
        }
    }

    pub fn verb(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Action::Ignore, false) => "ignored",
            (Action::Ignore, true) => "would ignore",
            (Action::Unignore, false) => "unignored",
            (Action::Unignore, true) => "would unignore",
            (Action::Unmatched, _) => "no rule",
        }
    }

    pub fn changes_anything(self) -> bool {
        self != Action::Unmatched
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    pub path: PathBuf,
    pub is_dir: bool,
    pub action: Action,
    pub rule: Option<Rule>,
}

impl Change {
    /// Perform the change on disk.
    pub fn apply(&self) -> anyhow::Result<()> {
        match self.action {
            Action::Ignore => dropbox::set_ignored(&self.path, true),
            Action::Unignore => dropbox::set_ignored(&self.path, false),
            Action::Unmatched => Ok(()),
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Synced files and folders seen while scanning
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub already_ignored: u64,
}

impl Plan {
    /// Scan `root` (whose children are governed by `rules`) for items whose
    /// ignore flag disagrees with the rules.
    pub fn scan(root: &Path, rules: Rules, unignore_unmatched: bool) -> Plan {
        let mut plan = Plan::default();
        plan.walk(root, rules, unignore_unmatched);
        plan
    }

    pub fn count(&self, action: Action) -> u64 {
        self.changes.iter().filter(|c| c.action == action).count() as u64
    }

    /// Whether applying the plan would change anything.
    pub fn is_noop(&self) -> bool {
        !self.changes.iter().any(|c| c.action.changes_anything())
    }

    fn walk(&mut self, dir: &Path, rules: Rules, unignore_unmatched: bool) {
        let mut entries =
            match std::fs::read_dir(dir).and_then(|it| it.collect::<Result<Vec<_>, _>>()) {
                Ok(entries) => entries,
                Err(err) => {
                    ui::warn(format!("skipping {}: {err}", ui::tilde(dir)));
                    return;
                }
            };
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let is_dir = meta.is_dir();
            let verdict = rules.check(&path, is_dir);
            let change = |action, rule: Option<&Rule>| Change {
                path: path.clone(),
                is_dir,
                action,
                rule: rule.cloned(),
            };

            let synced = match (verdict.should_ignore(), dropbox::is_ignored(&path)) {
                (true, false) => {
                    self.changes.push(change(Action::Ignore, verdict.rule()));
                    false
                }
                (true, true) => {
                    self.already_ignored += 1;
                    false
                }
                (false, true) if unignore_unmatched => {
                    self.changes.push(change(Action::Unignore, None));
                    true
                }
                (false, true) => {
                    self.changes.push(change(Action::Unmatched, verdict.rule()));
                    false
                }
                (false, false) => true,
            };

            if synced {
                self.bytes += meta.len();
                if is_dir {
                    self.dirs += 1;
                    self.walk(&path, rules.descend(&path), unignore_unmatched);
                } else {
                    self.files += 1;
                }
            }
        }
    }
}
