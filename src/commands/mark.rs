//! `ignore` and `unignore`: set or clear the ignore flag by hand.

use std::{
    io::{BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use owo_colors::Style;

use crate::{
    dropbox::{self, Dropbox},
    rules::{Rules, Verdict},
    ui::{self, Kind, Mark},
};

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// Files or folders to change
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Also apply to everything inside the given folders
    #[arg(short, long)]
    recursive: bool,

    /// Show what would change without touching anything
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Ignore paths already in Dropbox without asking (Dropbox deletes them online)
    #[arg(short, long)]
    yes: bool,
}

pub fn run(args: MarkArgs, ignore: bool) -> Result<()> {
    let dropbox = Dropbox::discover(false);
    let mut failures = 0;

    for input in &args.paths {
        if let Err(err) = mark(input, ignore, &args, &dropbox) {
            ui::error(&err);
            failures += 1;
        }
    }

    if failures > 0 {
        bail!(
            "{} could not be updated",
            ui::plural(failures, "path", "paths")
        );
    }
    Ok(())
}

fn mark(input: &Path, ignore: bool, args: &MarkArgs, dropbox: &Dropbox) -> Result<()> {
    std::fs::symlink_metadata(input).with_context(|| format!("can't open {}", input.display()))?;
    let path = std::path::absolute(input)?;
    let kind = Kind::of(&path);
    let shown = ui::entry_name(&input.display().to_string(), &kind, true);

    if dropbox.knows_roots() && dropbox.root_of(&path).is_none() {
        ui::warn(format!(
            "{} is not inside a Dropbox folder",
            ui::tilde(&path)
        ));
    }

    let changed_self = dropbox::is_ignored(&path) != ignore;
    if ignore && changed_self && in_dropbox(&path, dropbox) && !args.yes {
        let warning = format!(
            "{shown} is in Dropbox: ignoring it deletes it from dropbox.com and your \
             other computers (this copy stays)"
        );
        if args.dry_run {
            note(warning);
        } else if !confirm(&warning, kind.is_dir())? {
            println!(
                "{} {} {shown}",
                ui::dim("·"),
                ui::dim(format!("{:<14}", "left synced"))
            );
            return Ok(());
        }
    }
    if changed_self && !args.dry_run {
        dropbox::set_ignored(&path, ignore)?;
    }

    let mut inner = 0u64;
    if args.recursive && kind.is_dir() {
        for item in descendants(&path) {
            if dropbox::is_ignored(&item) != ignore {
                if !args.dry_run {
                    dropbox::set_ignored(&item, ignore)?;
                }
                inner += 1;
            }
        }
    }

    let (mark, verb) = match (ignore, args.dry_run) {
        (true, false) => (Mark::Ignored, "ignored"),
        (true, true) => (Mark::Ignored, "would ignore"),
        (false, false) => (Mark::Restored, "unignored"),
        (false, true) => (Mark::Restored, "would unignore"),
    };
    let inner_note = if inner > 0 {
        format!(
            "  {}",
            ui::dim(format!("+{} inside", ui::plural(inner, "item", "items")))
        )
    } else {
        String::new()
    };

    if changed_self || inner > 0 {
        println!(
            "{} {} {shown}{inner_note}",
            mark.icon(),
            ui::paint(format!("{verb:<14}"), mark.style())
        );
    } else {
        let already = if ignore {
            "already ignored"
        } else {
            "not ignored"
        };
        println!(
            "{} {} {shown}",
            ui::dim("·"),
            ui::dim(format!("{already:<14}"))
        );
    }

    // explain anything that will make the change ineffective or temporary
    if ignore {
        if let Some(ancestor) = dropbox.ignored_ancestor(&path) {
            note(format!(
                "already untracked: inside ignored {}",
                ui::tilde(&ancestor)
            ));
        }
    } else {
        let top = dropbox.root_of(&path);
        let rules = Rules::for_dir(path.parent().unwrap_or(&path), top);
        if let Verdict::Ignore(rule) = rules.check(&path, kind.is_dir()) {
            note(format!(
                "matches {}, so the next `di check` will ignore it again",
                rule.describe(path.parent())
            ));
        }
        if let Some(ancestor) = dropbox.ignored_ancestor(&path) {
            note(format!(
                "still untracked: inside ignored {} (unignore that folder too)",
                ui::tilde(&ancestor)
            ));
        }
    }
    Ok(())
}

/// Whether Dropbox holds `path` online, so ignoring it deletes the online copy.
fn in_dropbox(path: &Path, dropbox: &Dropbox) -> bool {
    dropbox
        .root_of(path)
        .is_some_and(|root| root != path && dropbox.ignored_ancestor(path).is_none())
}

/// Ask before deleting something online. Without a terminal to ask on, refuse.
fn confirm(warning: &str, is_dir: bool) -> Result<bool> {
    note(warning.to_string());
    if is_dir {
        note("to keep it online but remove it here, use `di exclude` instead".into());
    }
    if !std::io::stdin().is_terminal() {
        bail!("not ignoring without confirmation; pass --yes to ignore anyway");
    }
    print!("  Ignore it anyway? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn note(text: String) {
    println!(
        "  {} {}",
        Mark::Warning.icon(),
        ui::paint(text, Style::new().yellow())
    );
}

/// Every file and folder below `dir`, without following symlinks.
fn descendants(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            ui::warn(format!("skipping unreadable {}", ui::tilde(&dir)));
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path.clone());
            }
            out.push(path);
        }
    }
    out
}
