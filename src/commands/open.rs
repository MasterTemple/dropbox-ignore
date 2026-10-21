//! `open`: show files and folders on dropbox.com.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::{
    dropbox::{self, Dropbox},
    ui::{self, Kind, Mark},
};

#[derive(Debug, Args)]
pub struct OpenArgs {
    /// Files or folders to open; excluded (online only) folders work too
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Print the addresses instead of opening a browser
    #[arg(short, long)]
    print: bool,
}

pub fn run(args: OpenArgs) -> Result<()> {
    let mut dropbox = Dropbox::discover(true);
    if !dropbox.knows_roots() {
        bail!("could not find your Dropbox folder (no ~/.dropbox/info.json)");
    }

    let mut failures = 0;
    for input in &args.paths {
        let result = web_url(input, &mut dropbox).and_then(|url| {
            if args.print {
                println!("{url}");
            } else {
                dropbox::open_in_browser(&url)?;
                println!(
                    "{}",
                    Mark::Success.with(format!("opened {} on dropbox.com", ui::tilde(input)))
                );
            }
            Ok(())
        });
        if let Err(err) = result {
            ui::error(&err);
            failures += 1;
        }
    }
    if failures > 0 {
        bail!(
            "{} could not be opened",
            ui::plural(failures, "path", "paths")
        );
    }
    Ok(())
}

fn web_url(input: &Path, dropbox: &mut Dropbox) -> Result<String> {
    let (path, is_dir) = match std::fs::canonicalize(input) {
        Ok(path) => {
            let is_dir = Kind::of(&path).is_dir();
            (path, is_dir)
        }
        // excluded folders and their contents exist only online
        Err(err) => {
            let path = std::path::absolute(input)
                .with_context(|| format!("can't open {}", input.display()))?;
            if dropbox.is_excluded(&path) {
                (path, true)
            } else if dropbox.excluded_ancestor(&path).is_some() {
                // nothing on disk to ask, so guess from the name
                let is_dir = path.extension().is_none();
                (path, is_dir)
            } else {
                return Err(err).with_context(|| format!("can't open {}", input.display()));
            }
        }
    };

    if dropbox::is_ignored(&path) {
        bail!(
            "{} is ignored, so it isn't on dropbox.com",
            ui::tilde(&path)
        );
    }
    if let Some(dir) = dropbox.ignored_ancestor(&path) {
        bail!(
            "{} is inside ignored folder {}, so it isn't on dropbox.com",
            ui::tilde(&path),
            ui::tilde(&dir)
        );
    }
    dropbox.web_url(&path, is_dir)
}
