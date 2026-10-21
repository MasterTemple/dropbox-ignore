//! `exclude` and `include`: Dropbox selective sync, which keeps folders online
//! but removes them from this computer.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use owo_colors::Style;

use crate::{
    dropbox::{self, Dropbox, SyncStatus},
    ui::{self, Kind, Mark},
};

/// How many local-only items to name before giving up on a folder.
const LOCAL_ONLY_SHOWN: usize = 5;

#[derive(Debug, Args)]
pub struct ExcludeArgs {
    /// Folders to make online only; with none, list the excluded folders
    paths: Vec<PathBuf>,

    /// Exclude even if the folder is still syncing or holds ignored items
    /// (those exist only on this computer and will be deleted)
    #[arg(short, long)]
    force: bool,

    /// Show what would change without touching anything
    #[arg(short = 'n', long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
pub struct IncludeArgs {
    /// Excluded folders to download again
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Show what would change without touching anything
    #[arg(short = 'n', long)]
    dry_run: bool,
}

pub fn exclude(args: ExcludeArgs) -> Result<()> {
    let mut dropbox = connect()?;
    if args.paths.is_empty() {
        return list(&mut dropbox);
    }

    let mut ready = Vec::new();
    let mut failures = 0;
    for input in &args.paths {
        match check_exclude(input, &args, &mut dropbox) {
            Ok(Some(path)) => ready.push((input, path)),
            Ok(None) => {}
            Err(err) => {
                ui::error(&err);
                failures += 1;
            }
        }
    }

    if args.dry_run {
        for (input, _) in &ready {
            report(Mark::Excluded, "would exclude", input);
        }
    } else if !ready.is_empty() {
        let folders: Vec<PathBuf> = ready.iter().map(|(_, p)| p.clone()).collect();
        let excluded = dropbox.exclude(&folders)?;
        for (input, path) in &ready {
            if excluded.contains(path) {
                report(Mark::Excluded, "excluded", input);
            } else {
                ui::error(&anyhow::anyhow!(
                    "Dropbox did not exclude {}",
                    ui::tilde(path)
                ));
                failures += 1;
            }
        }
        if !excluded.is_empty() {
            println!(
                "  {}",
                ui::dim("Removed from this computer, kept in Dropbox. Undo with `di include`.")
            );
        }
    }

    finish(failures)
}

/// Validate one folder; `Ok(None)` when there is nothing to do.
fn check_exclude(
    input: &Path,
    args: &ExcludeArgs,
    dropbox: &mut Dropbox,
) -> Result<Option<PathBuf>> {
    let path = std::path::absolute(input)?;
    if dropbox.is_excluded(&path) {
        skip("already excluded", input);
        return Ok(None);
    }
    let path =
        std::fs::canonicalize(input).with_context(|| format!("can't open {}", input.display()))?;
    let shown = ui::tilde(&path);

    if !Kind::of(&path).is_dir() {
        bail!(
            "{shown} is not a folder: Dropbox on Linux can only make folders online only \
             (move the file into a folder and exclude that)"
        );
    }
    let Some(root) = dropbox.root_of(&path) else {
        bail!("{shown} is not inside a Dropbox folder");
    };
    if root == path {
        bail!("{shown} is the Dropbox folder itself");
    }
    if dropbox::is_ignored(&path) {
        bail!("{shown} is ignored, so it isn't in Dropbox: excluding it would delete it for good");
    }
    if let Some(ancestor) = dropbox.ignored_ancestor(&path) {
        bail!(
            "{shown} is inside ignored {}, so it isn't in Dropbox: excluding it would delete it for good",
            ui::tilde(&ancestor)
        );
    }

    if args.force {
        return Ok(Some(path));
    }
    match dropbox.sync_status(&path) {
        SyncStatus::UpToDate => {}
        status => bail!(
            "{shown} is not fully synced ({}); wait for Dropbox, or pass --force",
            crate::model::sync_text(&status)
        ),
    }
    let local = dropbox::ignored_within(&path, LOCAL_ONLY_SHOWN + 1);
    if !local.is_empty() {
        for item in local.iter().take(LOCAL_ONLY_SHOWN) {
            note(format!(
                "ignored, only on this computer: {}",
                ui::tilde(item)
            ));
        }
        if local.len() > LOCAL_ONLY_SHOWN {
            note("… and more".into());
        }
        bail!(
            "{shown} holds ignored items that excluding would delete; move them out, or pass --force"
        );
    }
    Ok(Some(path))
}

pub fn include(args: IncludeArgs) -> Result<()> {
    let mut dropbox = connect()?;
    let mut ready = Vec::new();
    let mut failures = 0;
    for input in &args.paths {
        let path = std::path::absolute(input)?;
        if dropbox.is_excluded(&path) {
            ready.push((input, path));
            continue;
        }
        failures += 1;
        let hint = match dropbox.excluded_ancestor(&path) {
            Some(ancestor) => format!(" (it's inside excluded {})", ui::tilde(&ancestor)),
            None => " (see `di exclude` for the list)".into(),
        };
        ui::error(&anyhow::anyhow!(
            "{} is not excluded{hint}",
            ui::tilde(&path)
        ));
    }

    if args.dry_run {
        for (input, _) in &ready {
            report(Mark::Restored, "would include", input);
        }
    } else if !ready.is_empty() {
        let folders: Vec<PathBuf> = ready.iter().map(|(_, p)| p.clone()).collect();
        let included = dropbox.include(&folders)?;
        for (input, path) in &ready {
            if included.contains(path) {
                report(Mark::Restored, "included", input);
            } else {
                ui::error(&anyhow::anyhow!(
                    "Dropbox did not include {}",
                    ui::tilde(path)
                ));
                failures += 1;
            }
        }
        if !included.is_empty() {
            println!(
                "  {}",
                ui::dim("Dropbox is downloading them to this computer.")
            );
        }
    }

    finish(failures)
}

fn list(dropbox: &mut Dropbox) -> Result<()> {
    let mut excluded = dropbox.excluded().to_vec();
    if excluded.is_empty() {
        println!("{}", ui::dim("No folders are excluded."));
        return Ok(());
    }
    excluded.sort();
    for path in &excluded {
        println!(
            "{} {}",
            Mark::Excluded.icon(),
            ui::entry_name(&ui::tilde(path), &Kind::Dir, true)
        );
    }
    println!();
    println!(
        "{}",
        ui::dim(format!(
            "{} online only. Download one again with `di include PATH`.",
            ui::plural(excluded.len() as u64, "folder", "folders")
        ))
    );
    Ok(())
}

fn connect() -> Result<Dropbox> {
    let dropbox = Dropbox::discover(true);
    if !dropbox.daemon_connected() {
        bail!("selective sync needs the Dropbox daemon, which isn't running or reachable");
    }
    Ok(dropbox)
}

fn report(mark: Mark, verb: &str, input: &Path) {
    println!(
        "{} {} {}",
        mark.icon(),
        ui::paint(format!("{verb:<14}"), mark.style()),
        ui::entry_name(&input.display().to_string(), &Kind::Dir, true)
    );
}

fn skip(why: &str, input: &Path) {
    println!(
        "{} {} {}",
        ui::dim("·"),
        ui::dim(format!("{why:<14}")),
        ui::entry_name(&input.display().to_string(), &Kind::Dir, true)
    );
}

fn note(text: String) {
    println!(
        "  {} {}",
        Mark::Warning.icon(),
        ui::paint(text, Style::new().yellow())
    );
}

fn finish(failures: u64) -> Result<()> {
    if failures > 0 {
        bail!(
            "{} could not be updated",
            ui::plural(failures, "folder", "folders")
        );
    }
    Ok(())
}
