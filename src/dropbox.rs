//! Everything that talks to Dropbox: the `com.dropbox.ignored` extended attribute,
//! the account folders listed in `~/.dropbox/info.json`, and the running daemon's
//! local command socket (the same one the official `dropbox filestatus` CLI uses).

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};

const XATTR_IGNORED: &str = "user.com.dropbox.ignored";

/// Whether the ignore attribute is set on `path` itself (not its ancestors).
pub fn is_ignored(path: &Path) -> bool {
    // symlinks and some filesystems can't carry user xattrs; treat as not ignored
    matches!(xattr::get(path, XATTR_IGNORED), Ok(Some(v)) if v == b"1")
}

pub fn set_ignored(path: &Path, ignored: bool) -> Result<()> {
    if ignored {
        xattr::set(path, XATTR_IGNORED, b"1")
            .with_context(|| format!("could not set ignore attribute on {}", path.display()))
    } else {
        xattr::remove(path, XATTR_IGNORED)
            .with_context(|| format!("could not clear ignore attribute on {}", path.display()))
    }
}

/// Sync status as reported by the Dropbox daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    UpToDate,
    Syncing,
    Unsyncable,
    /// Excluded through selective sync
    Excluded,
    Ignored,
    /// Not inside a Dropbox folder
    Unwatched,
    /// Daemon not reachable or not asked
    Unknown,
    Other(String),
}

impl SyncStatus {
    fn parse(s: &str) -> Self {
        match s {
            "up to date" => Self::UpToDate,
            "syncing" => Self::Syncing,
            "unsyncable" => Self::Unsyncable,
            "selsync" => Self::Excluded,
            "ignored" => Self::Ignored,
            "unwatched" => Self::Unwatched,
            other => Self::Other(other.to_string()),
        }
    }
}

/// Connection to the Dropbox daemon's command socket.
struct Daemon {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Daemon {
    fn connect() -> Option<Self> {
        let socket = home()?.join(".dropbox/command_socket");
        let stream = UnixStream::connect(socket).ok()?;
        let timeout = Some(Duration::from_secs(3));
        stream.set_read_timeout(timeout).ok()?;
        stream.set_write_timeout(timeout).ok()?;
        Some(Self {
            reader: BufReader::new(stream.try_clone().ok()?),
            writer: stream,
        })
    }

    /// Send a command; the outer `Result` is a transport failure, the inner one a
    /// command the daemon rejected (with its message).
    fn command(
        &mut self,
        name: &str,
        args: &[(&str, &str)],
    ) -> std::io::Result<Result<HashMap<String, Vec<String>>, String>> {
        let mut request = format!("{name}\n");
        for (key, value) in args {
            request.push_str(&format!("{key}\t{value}\n"));
        }
        request.push_str("done\n");
        self.writer.write_all(request.as_bytes())?;

        let first = self.read_line()?;
        let mut fields = HashMap::new();
        let mut message = Vec::new();
        loop {
            let line = self.read_line()?;
            if line == "done" {
                break;
            }
            let mut parts = line.split('\t').map(str::to_string);
            let key = parts.next().unwrap_or_default();
            message.push(key.clone());
            fields.insert(key, parts.collect());
        }
        Ok(match first.as_str() {
            "ok" => Ok(fields),
            _ => Err(message.join(" ")),
        })
    }

    fn read_line(&mut self) -> std::io::Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(line.trim_end_matches('\n').to_string())
    }
}

/// The Dropbox environment: account roots plus an optional daemon connection.
pub struct Dropbox {
    roots: Vec<PathBuf>,
    daemon: Option<Daemon>,
}

impl Dropbox {
    /// Discover Dropbox. With `query_daemon` false, no sync status is fetched.
    pub fn discover(query_daemon: bool) -> Self {
        Self {
            roots: account_roots(),
            daemon: if query_daemon {
                Daemon::connect()
            } else {
                None
            },
        }
    }

    pub fn daemon_connected(&self) -> bool {
        self.daemon.is_some()
    }

    /// Whether we know where the Dropbox folders are.
    pub fn knows_roots(&self) -> bool {
        !self.roots.is_empty()
    }

    /// The Dropbox folder containing `path`, if any.
    pub fn root_of(&self, path: &Path) -> Option<&Path> {
        self.roots
            .iter()
            .find(|root| path.starts_with(root))
            .map(PathBuf::as_path)
    }

    /// The nearest strict ancestor of `path` (inside its Dropbox folder) that is ignored.
    pub fn ignored_ancestor(&self, path: &Path) -> Option<PathBuf> {
        let root = self.root_of(path)?;
        path.ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(root) && *p != root)
            .find(|p| is_ignored(p))
            .map(Path::to_path_buf)
    }

    /// Overall daemon status headline, e.g. "Up to date" or "Syncing 3 files".
    pub fn daemon_status(&mut self) -> Option<String> {
        let fields = self.ask("get_dropbox_status", &[])?;
        let headline = fields.get("status")?.first()?;
        // the first line is already a summary; later ones name individual transfers
        Some(match headline.char_indices().nth(60) {
            Some((cut, _)) => format!("{}…", &headline[..cut]),
            None => headline.clone(),
        })
    }

    pub fn sync_status(&mut self, path: &Path) -> SyncStatus {
        let Some(path) = path.to_str() else {
            return SyncStatus::Unknown;
        };
        // the line-based protocol can't carry these
        if path.contains(['\t', '\n']) {
            return SyncStatus::Unknown;
        }
        self.ask("icon_overlay_file_status", &[("path", path)])
            .and_then(|f| f.get("status")?.first().map(|s| SyncStatus::parse(s)))
            .unwrap_or(SyncStatus::Unknown)
    }

    fn ask(&mut self, name: &str, args: &[(&str, &str)]) -> Option<HashMap<String, Vec<String>>> {
        let daemon = self.daemon.as_mut()?;
        match daemon.command(name, args) {
            Ok(result) => result.ok(),
            Err(_) => {
                // connection is broken; stop asking for the rest of the run
                self.daemon = None;
                None
            }
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Dropbox folders (personal and business) from `~/.dropbox/info.json`.
fn account_roots() -> Vec<PathBuf> {
    let Some(info) =
        home().and_then(|h| std::fs::read_to_string(h.join(".dropbox/info.json")).ok())
    else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Object(accounts)) = serde_json::from_str(&info) else {
        return Vec::new();
    };
    accounts
        .values()
        .filter_map(|account| account.get("path")?.as_str())
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)))
        .collect()
}
