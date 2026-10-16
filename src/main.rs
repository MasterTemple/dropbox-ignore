mod commands;
mod dropbox;
mod model;
mod rules;
mod ui;

use std::process::ExitCode;

use clap::{
    Parser, Subcommand,
    builder::styling::{AnsiColor, Effects, Styles},
};

use commands::{check::CheckArgs, mark::MarkArgs, status::StatusArgs, tree::TreeArgs};
use ui::{ColorChoice, IconSet};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Blue.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Blue.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

const EXAMPLES: &str = "\
Examples:
  di check ~/Dropbox            Apply every .dropboxignore rule
  di check ~/Dropbox --dry-run  Preview what would change
  di tree -L 2                  Show what Dropbox syncs in this folder
  di status target              Explain why a path is (not) synced
  di ignore node_modules        Stop syncing a folder right now

Rules use .gitignore syntax and can live in any folder inside Dropbox.";

/// Keep files out of Dropbox with .gitignore-style rules.
///
/// Put a `.dropboxignore` file in any folder inside Dropbox and run `check`
/// (e.g. from a timer) to flag matching files and folders as ignored.
#[derive(Debug, Parser)]
#[command(name = "di", version, styles = STYLES, after_help = EXAMPLES)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// When to use colors
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    color: ColorChoice,

    /// Glyph set for status icons
    #[arg(long, global = true, value_enum, default_value_t = IconSet::Unicode, env = "DROPBOXIGNORE_ICONS", value_name = "SET")]
    icons: IconSet,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Apply .dropboxignore rules: ignore every matching file and folder
    #[command(visible_alias = "sync")]
    Check(CheckArgs),

    /// Show a folder as a tree with tracking and sync status for every entry
    #[command(visible_alias = "ls")]
    Tree(TreeArgs),

    /// Explain the tracking state, sync status and matching rule of paths
    #[command(visible_alias = "st")]
    Status(StatusArgs),

    /// Stop syncing files or folders
    Ignore(MarkArgs),

    /// Resume syncing files or folders
    Unignore(MarkArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    ui::init(cli.color, cli.icons);

    let result = match cli.command {
        Command::Check(args) => commands::check::run(args),
        Command::Tree(args) => commands::tree::run(args),
        Command::Status(args) => commands::status::run(args),
        Command::Ignore(args) => commands::mark::run(args, true),
        Command::Unignore(args) => commands::mark::run(args, false),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        // the reader went away (e.g. piped into `head`): not an error
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(err) => {
            ui::error(&err);
            ExitCode::FAILURE
        }
    }
}
