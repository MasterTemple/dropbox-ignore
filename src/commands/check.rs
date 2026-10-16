use std::{
    io::{StdoutLock, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Result, bail};
use clap::Args;
use owo_colors::Style;

use crate::{
    dropbox::Dropbox,
    model::{State, Target},
    plan::{Action, Change, Plan},
    ui::{self, Kind, Mark},
};

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Folder to scan (rules from parent folders up to the Dropbox root also apply)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Show what would change without touching anything
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Also un-ignore items that are ignored but no longer match any rule
    #[arg(long)]
    unignore_unmatched: bool,

    /// Only print the final summary
    #[arg(short, long)]
    quiet: bool,

    /// Print one compact line for status bars: +NEW (UNMATCHED) SIZE
    #[arg(long, conflicts_with = "quiet")]
    summary: bool,
}

pub fn run(args: CheckArgs) -> Result<()> {
    let mut dropbox = Dropbox::discover(false);
    let target = Target::resolve(&args.path, &mut dropbox)?;
    if !target.kind.is_dir() {
        bail!("{} is not a folder", ui::tilde(&target.path));
    }
    match &target.state {
        State::Ignored(_) | State::InsideIgnored(_) => {
            if !args.summary {
                println!(
                    "{} {}",
                    target.state.mark().icon(),
                    ui::dim(format!(
                        "{} is not synced by Dropbox, nothing to check",
                        ui::tilde(&target.path)
                    ))
                );
            }
            return Ok(());
        }
        State::Outside => ui::warn(format!(
            "{} is not inside a Dropbox folder; ignore attributes will have no effect",
            ui::tilde(&target.path)
        )),
        _ => {}
    }

    let mut out = std::io::stdout().lock();
    let start = Instant::now();
    if !args.summary {
        let mut header = format!(
            "{} {}",
            ui::bold("Checking"),
            ui::paint(ui::tilde(&target.path), Style::new().blue().bold())
        );
        if args.dry_run {
            header.push_str(&format!(
                "  {}",
                ui::paint("dry run: nothing will be changed", Style::new().yellow())
            ));
        }
        writeln!(out, "{header}")?;
    }

    let plan = Plan::scan(&target.path, target.child_rules(), args.unignore_unmatched);

    let mut failures = 0;
    for change in &plan.changes {
        if change.action.changes_anything()
            && !args.dry_run
            && let Err(err) = change.apply()
        {
            ui::error(&err);
            failures += 1;
            continue;
        }
        if !args.quiet && !args.summary {
            report(&mut out, change, &target.path, args.dry_run)?;
        }
    }

    footer(&mut out, &args, &plan, failures, start)?;

    if failures > 0 {
        bail!(
            "{} could not be updated",
            ui::plural(failures, "item", "items")
        );
    }
    Ok(())
}

fn report(out: &mut StdoutLock, change: &Change, root: &Path, dry_run: bool) -> Result<()> {
    let mark = change.action.mark();
    let name = ui::pretty_path(&change.path, Some(root));
    let kind = if change.is_dir { Kind::Dir } else { Kind::File };
    let mut line = format!(
        "  {} {} {}",
        mark.icon(),
        ui::paint(format!("{:<14}", change.action.verb(dry_run)), mark.style()),
        ui::entry_name(&name, &kind, true)
    );
    if let Some(rule) = &change.rule {
        line.push_str(&format!("  {}", rule.describe(Some(root))));
    }
    writeln!(out, "{line}")?;
    Ok(())
}

fn footer(
    out: &mut StdoutLock,
    args: &CheckArgs,
    plan: &Plan,
    failures: u64,
    start: Instant,
) -> Result<()> {
    let ignored = plan.count(Action::Ignore);
    let unignored = plan.count(Action::Unignore);
    let unmatched = plan.count(Action::Unmatched);

    if args.summary {
        writeln!(
            out,
            "+{ignored} ({unmatched}) {:.2}GB",
            plan.bytes as f64 / 1024f64.powi(3)
        )?;
        return Ok(());
    }

    if !plan.changes.is_empty() {
        writeln!(out)?;
    }
    writeln!(
        out,
        "{} {} {} {} {}",
        if failures > 0 {
            Mark::Error.icon()
        } else {
            Mark::Success.icon()
        },
        ui::bold(format!(
            "Scanned {} in {}",
            ui::plural(plan.files, "file", "files"),
            ui::plural(plan.dirs, "folder", "folders")
        )),
        ui::dim(format!("in {}", ui::duration(start.elapsed()))),
        ui::separator(),
        ui::paint(
            format!("{} synced", ui::human_bytes(plan.bytes)),
            Style::new().green()
        )
    )?;

    let would = if args.dry_run { "would be " } else { "" };
    let parts: Vec<String> = [
        (ignored, Mark::Ignored, format!("{would}newly ignored")),
        (unignored, Mark::Restored, format!("{would}unignored")),
        (
            plan.already_ignored,
            Mark::Ignored,
            "already ignored".into(),
        ),
        (
            unmatched,
            Mark::IgnoredManual,
            "ignored without a rule".into(),
        ),
    ]
    .into_iter()
    .filter(|(n, ..)| *n > 0)
    .map(|(n, mark, text)| mark.with(format!("{} {text}", ui::thousands(n))))
    .collect();
    if !parts.is_empty() {
        writeln!(out, "  {}", parts.join("   "))?;
    }

    if plan.is_noop() {
        writeln!(
            out,
            "  {}",
            ui::dim("Everything already matches your .dropboxignore rules.")
        )?;
    }
    if unmatched > 0 {
        writeln!(
            out,
            "  {}",
            ui::dim(
                "Items ignored without a rule are left alone. Pass --unignore-unmatched to sync them again."
            )
        )?;
    }
    Ok(())
}
