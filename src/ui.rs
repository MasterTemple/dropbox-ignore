//! Terminal presentation: colors, glyphs and small formatting helpers.
//!
//! Every command draws from the same vocabulary of [`Mark`]s so that a given state
//! (synced, ignored, pending, ...) looks identical everywhere in the tool.

use std::{
    fmt::Display,
    io::IsTerminal,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use clap::ValueEnum;
use owo_colors::{OwoColorize, Style};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Color when writing to a terminal and NO_COLOR is unset
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IconSet {
    /// Nerd Font glyphs, including file and folder icons (needs a patched font)
    Nerd,
    /// Plain unicode symbols that render in any modern terminal
    Unicode,
    /// 7-bit ASCII only
    Ascii,
}

struct Settings {
    color: bool,
    icons: IconSet,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

pub fn init(color: ColorChoice, icons: IconSet) {
    let color = match color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
                && std::io::stdout().is_terminal()
        }
    };
    let _ = SETTINGS.set(Settings { color, icons });
}

fn settings() -> &'static Settings {
    SETTINGS.get_or_init(|| Settings {
        color: false,
        icons: IconSet::Unicode,
    })
}

pub fn icons() -> IconSet {
    settings().icons
}

/// Apply `style` to `text` if color output is enabled.
pub fn paint(text: impl Display, style: Style) -> String {
    if settings().color {
        text.style(style).to_string()
    } else {
        text.to_string()
    }
}

pub fn dim(text: impl Display) -> String {
    paint(text, Style::new().bright_black())
}

pub fn bold(text: impl Display) -> String {
    paint(text, Style::new().bold())
}

/// A visual state marker. Each one has a glyph per icon set and a color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Tracked by Dropbox and fully up to date
    Synced,
    /// Tracked, transfer in progress
    Syncing,
    /// Tracked, but Dropbox can't sync it
    Unsyncable,
    /// Excluded by Dropbox selective sync
    Excluded,
    /// Tracked; sync status unknown (daemon not reachable)
    Tracked,
    /// Ignored, backed by a .dropboxignore rule
    Ignored,
    /// Ignored, but no rule asks for it
    IgnoredManual,
    /// Matches a rule but is not ignored yet
    Pending,
    /// Not inside a Dropbox folder at all
    Outside,
    /// Something we could not determine
    Unknown,
    /// Item un-ignored by this tool
    Restored,
    Warning,
    Error,
    Success,
}

impl Mark {
    pub fn glyph(self) -> &'static str {
        use IconSet::*;
        use Mark::*;
        match (self, icons()) {
            (Synced, Nerd) => "\u{f058}",
            (Synced, Unicode) => "✓",
            (Synced, Ascii) => "+",
            (Syncing, Nerd) => "\u{f021}",
            (Syncing, Unicode) => "⟳",
            (Syncing, Ascii) => "~",
            (Unsyncable, Nerd) => "\u{f06a}",
            (Unsyncable, Unicode) => "✗",
            (Unsyncable, Ascii) => "x",
            (Excluded, Nerd) => "\u{f0c2}",
            (Excluded, Unicode) => "☁",
            (Excluded, Ascii) => "c",
            (Tracked, Nerd) => "\u{f111}",
            (Tracked, Unicode) => "●",
            (Tracked, Ascii) => "o",
            (Ignored | IgnoredManual, Nerd) => "\u{f05e}",
            (Ignored | IgnoredManual, Unicode) => "⊘",
            (Ignored | IgnoredManual, Ascii) => "-",
            (Pending, Nerd) => "\u{f017}",
            (Pending, Unicode) => "◌",
            (Pending, Ascii) => "*",
            (Outside, Nerd) => "\u{f070}",
            (Outside, Unicode) => "○",
            (Outside, Ascii) => ".",
            (Unknown, Nerd) => "\u{f059}",
            (Unknown, _) => "?",
            (Restored, Nerd) => "\u{f0e2}",
            (Restored, Unicode) => "↺",
            (Restored, Ascii) => "<",
            (Warning, Nerd) => "\u{f071}",
            (Warning, Unicode) => "▲",
            (Warning, Ascii) => "!",
            (Error, Nerd) => "\u{f057}",
            (Error, Unicode) => "✗",
            (Error, Ascii) => "x",
            (Success, Nerd) => "\u{f00c}",
            (Success, Unicode) => "✓",
            (Success, Ascii) => "+",
        }
    }

    pub fn style(self) -> Style {
        use Mark::*;
        match self {
            Synced | Success => Style::new().green(),
            Tracked => Style::new().green(),
            Syncing => Style::new().cyan(),
            Unsyncable | Error => Style::new().red().bold(),
            Excluded => Style::new().blue(),
            Ignored | Outside | Unknown => Style::new().bright_black(),
            IgnoredManual => Style::new().magenta(),
            Pending | Warning => Style::new().yellow(),
            Restored => Style::new().cyan(),
        }
    }

    /// The colored glyph.
    pub fn icon(self) -> String {
        paint(self.glyph(), self.style())
    }

    /// Colored glyph followed by colored text.
    pub fn with(self, text: impl Display) -> String {
        paint(format!("{} {text}", self.glyph()), self.style())
    }
}

/// What kind of filesystem entry something is, for icons and name styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink(Option<PathBuf>),
    Other,
}

impl Kind {
    pub fn of(path: &Path) -> Kind {
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => Kind::Symlink(std::fs::read_link(path).ok()),
            Ok(m) if m.is_dir() => Kind::Dir,
            Ok(m) if m.is_file() => Kind::File,
            _ => Kind::Other,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Kind::Dir)
    }

    pub fn noun(&self) -> &'static str {
        match self {
            Kind::Dir => "folder",
            Kind::File => "file",
            Kind::Symlink(_) => "symlink",
            Kind::Other => "special file",
        }
    }

    /// Icon shown in front of names. Only the nerd set has file-type icons, since
    /// emoji widths are too unreliable to keep trees aligned.
    pub fn icon(&self, name: &str) -> &'static str {
        if icons() != IconSet::Nerd {
            return "";
        }
        match self {
            Kind::Dir => "\u{f07b} ",
            Kind::Symlink(_) => "\u{f0c1} ",
            _ if name == ".dropboxignore" => "\u{f16b} ",
            _ => "\u{f15b} ",
        }
    }
}

/// Render an entry name with its kind icon, a trailing `/` for directories and
/// a style that reflects whether Dropbox tracks it.
pub fn entry_name(name: &str, kind: &Kind, tracked: bool) -> String {
    let icon = kind.icon(name);
    let (text, style) = match kind {
        Kind::Dir => (format!("{icon}{name}/"), Style::new().blue().bold()),
        Kind::Symlink(_) => (format!("{icon}{name}"), Style::new().cyan()),
        _ if name == ".dropboxignore" => (format!("{icon}{name}"), Style::new().yellow().bold()),
        _ => (format!("{icon}{name}"), Style::new()),
    };
    if tracked {
        paint(text, style)
    } else {
        paint(text, Style::new().bright_black())
    }
}

/// Display width of an [`entry_name`] before styling, for column alignment.
pub fn entry_name_width(name: &str, kind: &Kind) -> usize {
    let icon_width = if kind.icon(name).is_empty() { 0 } else { 2 };
    icon_width + name.width() + usize::from(kind.is_dir())
}

pub struct TreeGuides {
    pub branch: &'static str,
    pub last: &'static str,
    pub pipe: &'static str,
    pub blank: &'static str,
}

pub fn tree_guides() -> TreeGuides {
    match icons() {
        IconSet::Ascii => TreeGuides {
            branch: "|-- ",
            last: "`-- ",
            pipe: "|   ",
            blank: "    ",
        },
        _ => TreeGuides {
            branch: "├── ",
            last: "└── ",
            pipe: "│   ",
            blank: "    ",
        },
    }
}

pub fn arrow() -> &'static str {
    match icons() {
        IconSet::Ascii => "->",
        _ => "→",
    }
}

pub fn separator() -> String {
    dim(match icons() {
        IconSet::Ascii => "|",
        _ => "·",
    })
}

/// Print a warning line to stderr.
pub fn warn(msg: impl Display) {
    eprintln!("{} {msg}", Mark::Warning.with("warning:"));
}

/// Print an error and its causes to stderr.
pub fn error(err: &anyhow::Error) {
    eprintln!("{} {err}", Mark::Error.with("error:"));
    for cause in err.chain().skip(1) {
        eprintln!("  {} {cause}", dim("caused by:"));
    }
}

/// Show a path relative to `base` when it lives below it, otherwise shorten the
/// home directory to `~`.
pub fn pretty_path(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base {
        if path == base {
            return ".".into();
        }
        if let Ok(rel) = path.strip_prefix(base) {
            return rel.display().to_string();
        }
        if let Some(rel) = relative_upwards(path, base) {
            return rel.display().to_string();
        }
    }
    tilde(path)
}

/// `path` expressed relative to `base` when `path` is an ancestor or a file in an
/// ancestor of `base` (e.g. `../../.dropboxignore`).
fn relative_upwards(path: &Path, base: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let rest = base.strip_prefix(parent).ok()?;
    let mut out = PathBuf::new();
    for c in rest.components() {
        if matches!(c, Component::Normal(_)) {
            out.push("..");
        }
    }
    out.push(path.file_name()?);
    Some(out)
}

pub fn tilde(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return if rel.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rel.display())
        };
    }
    path.display().to_string()
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

pub fn duration(d: Duration) -> String {
    if d.as_secs() >= 60 {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    } else if d.as_millis() >= 1000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}ms", d.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn relative_paths() {
        let base = Path::new("/d/a/b");
        assert_eq!(pretty_path(Path::new("/d/a/b/c/x"), Some(base)), "c/x");
        assert_eq!(
            pretty_path(Path::new("/d/a/.dropboxignore"), Some(base)),
            "../.dropboxignore"
        );
        assert_eq!(pretty_path(Path::new("/d/a/b"), Some(base)), ".");
    }
}
