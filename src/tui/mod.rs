//! `di tui`: an interactive tree of what Dropbox syncs.

mod app;
mod view;

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::Args;
use ratatui::{
    DefaultTerminal,
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
        execute,
    },
};

use crate::{model::Filter, ui};
use app::App;

/// How often sync status on screen is refreshed.
const TICK: Duration = Duration::from_millis(1500);

#[derive(Debug, Args)]
pub struct TuiArgs {
    /// Folder to open
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Start with dotfiles hidden (toggle with `.`)
    #[arg(long)]
    no_hidden: bool,

    /// Hide entries whose name matches GLOB (repeatable, or comma-separated in DI_HIDE)
    #[arg(
        short = 'I',
        long = "hide",
        value_name = "GLOB",
        env = "DI_HIDE",
        value_delimiter = ','
    )]
    hide: Vec<String>,

    /// Skip asking the Dropbox daemon for sync status
    #[arg(long)]
    no_sync: bool,
}

pub fn run(args: TuiArgs) -> Result<()> {
    let filter = Filter::new(!args.no_hidden, false, &args.hide)?;
    let mut app = App::new(&args.path, filter, !args.no_sync)?;

    ui::capture_warnings();
    let mut terminal = enter()?;
    let result = event_loop(&mut terminal, &mut app);
    leave();
    result
}

fn enter() -> Result<DefaultTerminal> {
    let terminal = ratatui::try_init().context("can't start the terminal UI")?;
    execute!(std::io::stdout(), EnableMouseCapture)?;
    Ok(terminal)
}

fn leave() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    while !app.quit {
        terminal.draw(|frame| view::draw(frame, app))?;

        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                _ => {}
            }
        }
        if last_tick.elapsed() >= TICK {
            app.tick();
            last_tick = Instant::now();
        }

        if let Some(file) = app.edit_request.take() {
            leave();
            let outcome = open_editor(&file);
            *terminal = enter()?;
            app.reload();
            match outcome {
                Ok(()) => app.notify(
                    ui::Mark::Success,
                    format!("Reloaded rules from {}", ui::tilde(&file)),
                ),
                Err(err) => app.notify(ui::Mark::Error, format!("{err:#}")),
            }
        }
    }
    Ok(())
}

fn open_editor(file: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    // allow editors configured with arguments, like `code --wait`
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(file)
        .status()
        .with_context(|| format!("couldn't launch editor `{editor}`"))?;
    if !status.success() {
        anyhow::bail!("editor exited with {status}");
    }
    Ok(())
}
