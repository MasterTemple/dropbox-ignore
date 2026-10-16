use std::{
    io::{StdoutLock, Write},
    path::{Path, PathBuf},
};

use anyhow::Result;
use clap::Args;
use unicode_width::UnicodeWidthStr;

use crate::{
    dropbox::{Dropbox, SyncStatus},
    model::{State, Target, daemon_mark},
    rules::Rules,
    ui::{self, Kind, Mark},
};

/// Names wider than this don't push the status column further right.
const MAX_NAME_COLUMN: usize = 40;

#[derive(Debug, Args)]
pub struct TreeArgs {
    /// Folder to display
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Descend at most this many levels
    #[arg(short = 'L', long, value_name = "LEVELS")]
    depth: Option<usize>,

    /// List folders only
    #[arg(short, long)]
    dirs_only: bool,

    /// Hide dotfiles and dotfolders
    #[arg(long)]
    no_hidden: bool,

    /// Skip asking the Dropbox daemon for sync status (faster)
    #[arg(long)]
    no_sync: bool,
}

#[derive(Default)]
struct Counts {
    dirs: u64,
    files: u64,
    synced: u64,
    syncing: u64,
    unsyncable: u64,
    excluded: u64,
    tracked: u64,
    ignored: u64,
    ignored_manual: u64,
    pending: u64,
}

impl Counts {
    fn add(&mut self, kind: &Kind, state: &State) {
        if kind.is_dir() {
            self.dirs += 1;
        } else {
            self.files += 1;
        }
        let counter = match state.mark() {
            Mark::Synced => &mut self.synced,
            Mark::Syncing => &mut self.syncing,
            Mark::Unsyncable => &mut self.unsyncable,
            Mark::Excluded => &mut self.excluded,
            Mark::Ignored => &mut self.ignored,
            Mark::IgnoredManual => &mut self.ignored_manual,
            Mark::Pending => &mut self.pending,
            _ => &mut self.tracked,
        };
        *counter += 1;
    }
}

struct Node {
    name: String,
    path: PathBuf,
    kind: Kind,
    state: State,
}

struct Tree<'a> {
    args: &'a TreeArgs,
    dropbox: Dropbox,
    base: PathBuf,
    counts: Counts,
    out: StdoutLock<'static>,
}

pub fn run(args: TreeArgs) -> Result<()> {
    let mut dropbox = Dropbox::discover(!args.no_sync);
    let target = Target::resolve(&args.path, &mut dropbox)?;
    let daemon_status = dropbox.daemon_status();

    let mut tree = Tree {
        args: &args,
        dropbox,
        base: target.path.clone(),
        counts: Counts::default(),
        out: std::io::stdout().lock(),
    };

    let name = ui::tilde(&target.path);
    let label = target
        .state
        .label(target.path.parent())
        .map(|l| format!("  {l}"))
        .unwrap_or_default();
    writeln!(
        tree.out,
        "{} {}{label}",
        target.state.mark().icon(),
        ui::entry_name(&name, &target.kind, target.state.is_tracked())
    )?;

    // per the Dropbox model, nothing below an untracked folder can be tracked
    let explore = target.kind.is_dir() && target.state.is_tracked();
    if explore {
        let rules = target.child_rules();
        tree.walk(&target.path, &rules, "", 1)?;
    }

    tree.footer(&target, explore.then_some(daemon_status).flatten(), explore)
}

impl Tree<'_> {
    fn walk(&mut self, dir: &Path, rules: &Rules, prefix: &str, depth: usize) -> Result<()> {
        let guides = ui::tree_guides();
        let nodes = match self.read_children(dir, rules) {
            Ok(nodes) => nodes,
            Err(err) => {
                writeln!(
                    self.out,
                    "{prefix}{}{}",
                    guides.last,
                    Mark::Error.with(format!("can't read folder: {err}"))
                )?;
                return Ok(());
            }
        };

        let column = nodes
            .iter()
            .map(|n| ui::entry_name_width(&n.name, &n.kind))
            .filter(|w| *w <= MAX_NAME_COLUMN)
            .max()
            .unwrap_or(0);

        for (i, node) in nodes.iter().enumerate() {
            let last = i + 1 == nodes.len();
            self.counts.add(&node.kind, &node.state);

            let mut line = format!(
                "{prefix}{}{} {}",
                ui::dim(if last { guides.last } else { guides.branch }),
                node.state.mark().icon(),
                ui::entry_name(&node.name, &node.kind, node.state.is_tracked())
            );
            let mut width = ui::entry_name_width(&node.name, &node.kind);
            if let Kind::Symlink(target) = &node.kind {
                let target = target
                    .as_deref()
                    .map(ui::tilde)
                    .unwrap_or_else(|| "?".into());
                let suffix = format!(" {} {target}", ui::arrow());
                width += suffix.width();
                line.push_str(&ui::dim(suffix));
            }
            if let Some(label) = node.state.label(Some(&self.base)) {
                let pad = column.saturating_sub(width) + 2;
                line.push_str(&" ".repeat(pad));
                line.push_str(&label);
            }
            writeln!(self.out, "{line}")?;

            let within_depth = self.args.depth.is_none_or(|max| depth < max);
            if node.kind.is_dir() && node.state.is_tracked() && within_depth {
                let child_prefix = format!(
                    "{prefix}{}",
                    ui::dim(if last { guides.blank } else { guides.pipe })
                );
                let child_rules = rules.descend(&node.path);
                self.walk(&node.path, &child_rules, &child_prefix, depth + 1)?;
            }
        }
        Ok(())
    }

    fn read_children(&mut self, dir: &Path, rules: &Rules) -> std::io::Result<Vec<Node>> {
        let mut nodes = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if self.args.no_hidden && name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let kind = Kind::of(&path);
            if self.args.dirs_only && !kind.is_dir() {
                continue;
            }
            let state = State::classify(&path, kind.is_dir(), rules, &mut self.dropbox);
            nodes.push(Node {
                name,
                path,
                kind,
                state,
            });
        }
        nodes.sort_by_cached_key(|n| (!n.kind.is_dir(), n.name.to_lowercase()));
        Ok(nodes)
    }

    fn footer(
        mut self,
        target: &Target,
        daemon_status: Option<String>,
        explored: bool,
    ) -> Result<()> {
        let c = &self.counts;
        let out = &mut self.out;
        writeln!(out)?;

        if !explored {
            let hint = match target.state {
                State::Outside | State::Tracked(SyncStatus::Unwatched) => {
                    "This path is not inside a Dropbox folder, so nothing in it syncs."
                }
                _ if target.kind.is_dir() => {
                    "Untracked folders are not explored: Dropbox ignores everything inside them."
                }
                _ => return Ok(()),
            };
            writeln!(out, "{}", ui::dim(hint))?;
            return Ok(());
        }

        let mut totals = vec![ui::plural(c.dirs, "folder", "folders")];
        if !self.args.dirs_only {
            totals.push(ui::plural(c.files, "file", "files"));
        }
        let mut line = ui::bold(totals.join(", "));
        if let Some(status) = daemon_status {
            line.push_str(&format!(
                "  {}  {} {}",
                ui::separator(),
                ui::dim("Dropbox:"),
                daemon_mark(&status).with(&status)
            ));
        }
        writeln!(out, "{line}")?;

        let summary: Vec<String> = [
            (c.synced, Mark::Synced, "synced"),
            (c.tracked, Mark::Tracked, "tracked"),
            (c.syncing, Mark::Syncing, "syncing"),
            (c.unsyncable, Mark::Unsyncable, "can't sync"),
            (c.excluded, Mark::Excluded, "excluded"),
            (c.ignored, Mark::Ignored, "ignored"),
            (c.ignored_manual, Mark::IgnoredManual, "ignored manually"),
            (c.pending, Mark::Pending, "pending"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, mark, text)| mark.with(format!("{} {text}", ui::thousands(n))))
        .collect();
        if !summary.is_empty() {
            writeln!(out, "{}", summary.join("   "))?;
        }

        if !self.args.no_sync && !self.dropbox.daemon_connected() && target.state.is_tracked() {
            writeln!(
                out,
                "{}",
                ui::dim("Dropbox daemon not reachable, so sync status is unavailable.")
            )?;
        }
        if c.pending > 0 {
            writeln!(
                out,
                "{} {} {}",
                Mark::Pending.icon(),
                ui::dim(format!(
                    "{} a .dropboxignore rule but {} still synced. Apply with:",
                    ui::plural(c.pending, "item matches", "items match"),
                    if c.pending == 1 { "is" } else { "are" }
                )),
                ui::paint(
                    format!("di check {}", shell_path(&target.path)),
                    owo_colors::Style::new().cyan()
                )
            )?;
        }
        Ok(())
    }
}

/// A path the user can paste back into a shell.
pub fn shell_path(path: &Path) -> String {
    let short = ui::tilde(path);
    if short.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c)) {
        // quoting disables `~` expansion, so spell out the full path
        format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
    } else {
        short
    }
}
