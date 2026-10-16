//! `status`: a detailed card per path.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use crate::{
    dropbox::{Dropbox, SyncStatus},
    model::{State, Target, daemon_mark, sync_mark, sync_text},
    rules::Verdict,
    ui::{self, Mark},
};

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Files or folders to inspect
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
}

pub fn run(args: StatusArgs) -> Result<()> {
    let mut dropbox = Dropbox::discover(true);

    match dropbox.daemon_status() {
        Some(status) => println!(
            "{}  {}\n",
            ui::bold("Dropbox"),
            daemon_mark(&status).with(&status)
        ),
        None => println!(
            "{}  {}\n",
            ui::bold("Dropbox"),
            Mark::Unknown.with("daemon not reachable, sync status unavailable")
        ),
    }

    let mut failures = 0;
    for (i, input) in args.paths.iter().enumerate() {
        if i > 0 {
            println!();
        }
        match Target::resolve(input, &mut dropbox) {
            Ok(target) => card(&target, &dropbox),
            Err(err) => {
                ui::error(&err);
                failures += 1;
            }
        }
    }
    if failures > 0 {
        bail!(
            "{} could not be inspected",
            ui::plural(failures, "path", "paths")
        );
    }
    Ok(())
}

fn card(target: &Target, dropbox: &Dropbox) {
    let row = |key: &str, value: String| println!("  {} {value}", ui::dim(format!("{key:<9}")));
    let state = &target.state;

    println!(
        "{} {}  {}",
        state.mark().icon(),
        ui::entry_name(&ui::tilde(&target.path), &target.kind, state.is_tracked()),
        ui::dim(target.kind.noun())
    );

    let tracking = match state {
        State::Tracked(_) => Mark::Tracked.with("tracked"),
        State::Pending(_) => {
            Mark::Pending.with("tracked, but a rule says it should be ignored (run `di check`)")
        }
        State::Ignored(Some(_)) => Mark::Ignored.with("ignored"),
        State::Ignored(None) => Mark::IgnoredManual.with("ignored, though no rule asks for it"),
        State::InsideIgnored(dir) => Mark::Ignored.with(format!(
            "untracked, inside ignored folder {}",
            ui::tilde(dir)
        )),
        State::Outside => Mark::Outside.with("not inside a Dropbox folder"),
    };
    row("Tracking", tracking);

    match state {
        State::Tracked(SyncStatus::Unknown) | State::Pending(_) if !dropbox.daemon_connected() => {
            row("Sync", Mark::Unknown.with("unknown"))
        }
        State::Tracked(status) => row("Sync", sync_mark(status).with(sync_text(status))),
        _ => {}
    }

    if !matches!(state, State::Outside | State::InsideIgnored(_)) {
        let rule = match target.own_rules().check(&target.path, target.kind.is_dir()) {
            Verdict::Unmatched => ui::dim("no matching rule"),
            Verdict::Ignore(rule) => format!(
                "{}  {}",
                rule.describe(target.path.parent()),
                ui::dim("(ignore)")
            ),
            Verdict::Keep(rule) => format!(
                "{}  {}",
                rule.describe(target.path.parent()),
                ui::dim("(keep synced)")
            ),
        };
        row("Rule", rule);
    }

    if let Some(root) = &target.dropbox_root {
        row("Dropbox", ui::tilde(root));
    }
}
