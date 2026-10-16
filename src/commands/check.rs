use std::{
    io::{StdoutLock, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Result, bail};
use clap::Args;
use owo_colors::Style;

use crate::{
    dropbox::{self, Dropbox},
    model::{State, Target},
    rules::{Rule, Rules},
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

#[derive(Default)]
struct Stats {
    files: u64,
    dirs: u64,
    bytes: u64,
    newly_ignored: u64,
    already_ignored: u64,
    unmatched: u64,
    unignored: u64,
    failures: u64,
}

struct Check<'a> {
    args: &'a CheckArgs,
    root: PathBuf,
    stats: Stats,
    out: StdoutLock<'static>,
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

    let mut check = Check {
        args: &args,
        root: target.path.clone(),
        stats: Stats::default(),
        out: std::io::stdout().lock(),
    };

    let start = Instant::now();
    if !args.summary {
        let mut header = format!(
            "{} {}",
            ui::bold("Checking"),
            ui::paint(ui::tilde(&check.root), Style::new().blue().bold())
        );
        if args.dry_run {
            header.push_str(&format!(
                "  {}",
                ui::paint("dry run: nothing will be changed", Style::new().yellow())
            ));
        }
        writeln!(check.out, "{header}")?;
    }

    check.walk(&target.path, target.child_rules())?;
    check.footer(start)?;

    if check.stats.failures > 0 {
        bail!(
            "{} could not be updated",
            ui::plural(check.stats.failures, "item", "items")
        );
    }
    Ok(())
}

impl Check<'_> {
    fn walk(&mut self, dir: &Path, rules: Rules) -> Result<()> {
        let mut entries =
            match std::fs::read_dir(dir).and_then(|it| it.collect::<Result<Vec<_>, _>>()) {
                Ok(entries) => entries,
                Err(err) => {
                    ui::warn(format!("skipping {}: {err}", ui::tilde(dir)));
                    return Ok(());
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

            match (verdict.should_ignore(), dropbox::is_ignored(&path)) {
                (true, false) => {
                    let ok = self.apply(&path, true);
                    if ok {
                        self.stats.newly_ignored += 1;
                    }
                    let verb = if self.args.dry_run {
                        "would ignore"
                    } else {
                        "ignored"
                    };
                    self.report(Mark::Ignored, verb, &path, is_dir, verdict.rule())?;
                }
                (true, true) => self.stats.already_ignored += 1,
                (false, true) if self.args.unignore_unmatched => {
                    if self.apply(&path, false) {
                        self.stats.unignored += 1;
                    }
                    let verb = if self.args.dry_run {
                        "would unignore"
                    } else {
                        "unignored"
                    };
                    self.report(Mark::Restored, verb, &path, is_dir, None)?;
                    self.count(&path, &meta, &rules)?;
                }
                (false, true) => {
                    self.stats.unmatched += 1;
                    self.report(
                        Mark::IgnoredManual,
                        "no rule",
                        &path,
                        is_dir,
                        verdict.rule(),
                    )?;
                }
                (false, false) => self.count(&path, &meta, &rules)?,
            }
        }
        Ok(())
    }

    /// Account for a synced entry, descending into folders.
    fn count(&mut self, path: &Path, meta: &std::fs::Metadata, rules: &Rules) -> Result<()> {
        self.stats.bytes += meta.len();
        if meta.is_dir() {
            self.stats.dirs += 1;
            self.walk(path, rules.descend(path))?;
        } else {
            self.stats.files += 1;
        }
        Ok(())
    }

    /// Set the ignore flag unless this is a dry run. Returns whether it succeeded.
    fn apply(&mut self, path: &Path, ignored: bool) -> bool {
        if self.args.dry_run {
            return true;
        }
        match dropbox::set_ignored(path, ignored) {
            Ok(()) => true,
            Err(err) => {
                self.stats.failures += 1;
                ui::error(&err);
                false
            }
        }
    }

    fn report(
        &mut self,
        mark: Mark,
        verb: &str,
        path: &Path,
        is_dir: bool,
        rule: Option<&Rule>,
    ) -> Result<()> {
        if self.args.quiet || self.args.summary {
            return Ok(());
        }
        let name = ui::pretty_path(path, Some(&self.root));
        let kind = if is_dir { Kind::Dir } else { Kind::File };
        let mut line = format!(
            "  {} {} {}",
            mark.icon(),
            ui::paint(format!("{verb:<14}"), mark.style()),
            ui::entry_name(&name, &kind, true)
        );
        if let Some(rule) = rule {
            line.push_str(&format!("  {}", rule.describe(Some(&self.root))));
        }
        writeln!(self.out, "{line}")?;
        Ok(())
    }

    fn footer(&mut self, start: Instant) -> Result<()> {
        let s = &self.stats;
        let out = &mut self.out;

        if self.args.summary {
            writeln!(
                out,
                "+{} ({}) {:.2}GB",
                s.newly_ignored,
                s.unmatched,
                s.bytes as f64 / 1024f64.powi(3)
            )?;
            return Ok(());
        }

        let changed = s.newly_ignored + s.unignored + s.failures > 0;
        if changed || s.unmatched > 0 {
            writeln!(out)?;
        }
        writeln!(
            out,
            "{} {} {} {} {}",
            if s.failures > 0 {
                Mark::Error.icon()
            } else {
                Mark::Success.icon()
            },
            ui::bold(format!(
                "Scanned {} in {}",
                ui::plural(s.files, "file", "files"),
                ui::plural(s.dirs, "folder", "folders")
            )),
            ui::dim(format!("in {}", ui::duration(start.elapsed()))),
            ui::separator(),
            ui::paint(
                format!("{} synced", ui::human_bytes(s.bytes)),
                Style::new().green()
            )
        )?;

        let would = if self.args.dry_run { "would be " } else { "" };
        let parts: Vec<String> = [
            (
                s.newly_ignored,
                Mark::Ignored,
                format!("{would}newly ignored"),
            ),
            (s.unignored, Mark::Restored, format!("{would}unignored")),
            (s.already_ignored, Mark::Ignored, "already ignored".into()),
            (
                s.unmatched,
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

        if !changed {
            writeln!(
                out,
                "  {}",
                ui::dim("Everything already matches your .dropboxignore rules.")
            )?;
        }
        if s.unmatched > 0 && !self.args.unignore_unmatched {
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
}
