//! Classifying paths into the states the tool reports on.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::{
    dropbox::{self, Dropbox, SyncStatus},
    rules::{Rule, Rules, Verdict},
    ui::{self, Kind, Mark},
};

/// Where a path stands with Dropbox.
#[derive(Debug, Clone)]
pub enum State {
    /// Dropbox tracks it
    Tracked(SyncStatus),
    /// Still tracked, but a rule says it should be ignored
    Pending(Rule),
    /// Carries the ignore attribute; `None` when no rule asks for it
    Ignored(Option<Rule>),
    /// Untracked because this ancestor folder is ignored
    InsideIgnored(PathBuf),
    /// Not inside any Dropbox folder
    Outside,
}

impl State {
    /// Classify a path given the rules of its parent directory.
    pub fn classify(path: &Path, is_dir: bool, rules: &Rules, dropbox: &mut Dropbox) -> State {
        let verdict = rules.check(path, is_dir);
        if dropbox::is_ignored(path) {
            return State::Ignored(match verdict {
                Verdict::Ignore(rule) => Some(rule),
                _ => None,
            });
        }
        match verdict {
            Verdict::Ignore(rule) => State::Pending(rule),
            _ => State::Tracked(dropbox.sync_status(path)),
        }
    }

    /// Whether Dropbox currently syncs this path (and so it is worth descending into).
    pub fn is_tracked(&self) -> bool {
        matches!(self, State::Tracked(_) | State::Pending(_))
    }

    pub fn mark(&self) -> Mark {
        match self {
            State::Tracked(status) => sync_mark(status),
            State::Pending(_) => Mark::Pending,
            State::Ignored(Some(_)) | State::InsideIgnored(_) => Mark::Ignored,
            State::Ignored(None) => Mark::IgnoredManual,
            State::Outside => Mark::Outside,
        }
    }

    /// Short annotation for tree rows; `None` for the unremarkable "synced" case.
    pub fn label(&self, base: Option<&Path>) -> Option<String> {
        let mark = self.mark();
        let text = match self {
            State::Tracked(SyncStatus::UpToDate | SyncStatus::Unknown) => return None,
            State::Tracked(status) => ui::paint(sync_text(status), mark.style()),
            State::Pending(rule) => format!(
                "{} {}",
                ui::paint("pending ignore", mark.style()),
                rule.describe(base)
            ),
            State::Ignored(Some(rule)) => {
                format!(
                    "{} {}",
                    ui::paint("ignored", mark.style()),
                    rule.describe(base)
                )
            }
            State::Ignored(None) => ui::paint("ignored manually", mark.style()),
            State::InsideIgnored(dir) => format!(
                "{} {}",
                ui::paint("untracked", mark.style()),
                ui::dim(format!("inside ignored {}", ui::tilde(dir)))
            ),
            State::Outside => ui::paint("not in Dropbox", mark.style()),
        };
        Some(text)
    }
}

pub fn sync_mark(status: &SyncStatus) -> Mark {
    match status {
        SyncStatus::UpToDate => Mark::Synced,
        SyncStatus::Syncing => Mark::Syncing,
        SyncStatus::Unsyncable => Mark::Unsyncable,
        SyncStatus::Excluded => Mark::Excluded,
        SyncStatus::Ignored => Mark::Ignored,
        SyncStatus::Unwatched => Mark::Outside,
        SyncStatus::Unknown => Mark::Tracked,
        SyncStatus::Other(_) => Mark::Unknown,
    }
}

/// Mark for the daemon's overall status headline.
pub fn daemon_mark(headline: &str) -> Mark {
    let lower = headline.to_lowercase();
    if lower.starts_with("up to date") {
        Mark::Synced
    } else if ["sync", "upload", "download", "index", "start"]
        .iter()
        .any(|w| lower.contains(w))
    {
        Mark::Syncing
    } else {
        Mark::Unknown
    }
}

pub fn sync_text(status: &SyncStatus) -> String {
    match status {
        SyncStatus::UpToDate => "up to date".into(),
        SyncStatus::Syncing => "syncing".into(),
        SyncStatus::Unsyncable => "can't sync".into(),
        SyncStatus::Excluded => "excluded by selective sync".into(),
        SyncStatus::Ignored => "ignored".into(),
        SyncStatus::Unwatched => "not watched by Dropbox".into(),
        SyncStatus::Unknown => "unknown".into(),
        SyncStatus::Other(s) => s.clone(),
    }
}

/// A path given on the command line, resolved and classified.
pub struct Target {
    pub path: PathBuf,
    pub kind: Kind,
    pub state: State,
    pub dropbox_root: Option<PathBuf>,
}

impl Target {
    pub fn resolve(input: &Path, dropbox: &mut Dropbox) -> Result<Self> {
        let path = std::fs::canonicalize(input)
            .with_context(|| format!("can't open {}", input.display()))?;
        let kind = Kind::of(&path);
        let dropbox_root = dropbox.root_of(&path).map(Path::to_path_buf);

        let state = if dropbox.knows_roots() && dropbox_root.is_none() {
            State::Outside
        } else if let Some(ancestor) = dropbox.ignored_ancestor(&path) {
            State::InsideIgnored(ancestor)
        } else if Some(path.as_path()) == dropbox_root.as_deref() {
            State::Tracked(dropbox.sync_status(&path))
        } else {
            let parent = path.parent().unwrap_or(&path);
            let rules = Rules::for_dir(parent, dropbox_root.as_deref());
            State::classify(&path, kind.is_dir(), &rules, dropbox)
        };

        Ok(Self {
            path,
            kind,
            state,
            dropbox_root,
        })
    }

    /// Rules applying to this target's children.
    pub fn child_rules(&self) -> Rules {
        Rules::for_dir(&self.path, self.dropbox_root.as_deref())
    }

    /// Rules applying to the target itself.
    pub fn own_rules(&self) -> Rules {
        let parent = self.path.parent().unwrap_or(&self.path);
        Rules::for_dir(parent, self.dropbox_root.as_deref())
    }
}

/// One directory entry, classified.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub state: State,
}

/// Which entries to show when listing folders.
#[derive(Clone)]
pub struct Filter {
    pub show_dotfiles: bool,
    pub dirs_only: bool,
    hide: GlobSet,
}

impl Filter {
    /// `hide` holds globs matched against entry names, e.g. `.git` or `*.swp`.
    pub fn new(show_dotfiles: bool, dirs_only: bool, hide: &[String]) -> Result<Self> {
        let mut set = GlobSetBuilder::new();
        for pattern in hide.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            // allow `.git/` to mean the folder, like in ignore files
            let glob = Glob::new(pattern.trim_end_matches('/'))
                .with_context(|| format!("invalid --hide pattern `{pattern}`"))?;
            set.add(glob);
        }
        Ok(Self {
            show_dotfiles,
            dirs_only,
            hide: set.build()?,
        })
    }

    pub fn allows(&self, name: &str, kind: &Kind) -> bool {
        (self.show_dotfiles || !name.starts_with('.'))
            && (!self.dirs_only || kind.is_dir())
            && !self.hide.is_match(name)
    }
}

/// The visible children of `dir`, folders first, plus how many were filtered out.
pub fn list_children(
    dir: &Path,
    rules: &Rules,
    dropbox: &mut Dropbox,
    filter: &Filter,
) -> std::io::Result<(Vec<Node>, usize)> {
    let mut nodes = Vec::new();
    let mut filtered = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let kind = Kind::of(&path);
        if !filter.allows(&name, &kind) {
            filtered += 1;
            continue;
        }
        let state = State::classify(&path, kind.is_dir(), rules, dropbox);
        nodes.push(Node {
            name,
            path,
            kind,
            state,
        });
    }
    nodes.sort_by_cached_key(|n| (!n.kind.is_dir(), n.name.to_lowercase()));
    Ok((nodes, filtered))
}
