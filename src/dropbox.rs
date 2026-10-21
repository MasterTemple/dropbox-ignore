//! Everything that talks to Dropbox: the `com.dropbox.ignored` extended attribute,
//! the account folders listed in `~/.dropbox/info.json`, and the running daemon's
//! local command socket (the same one the official `dropbox filestatus` and
//! `dropbox exclude` CLI commands use).

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};

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

    /// Change how long to wait for replies; `None` waits forever.
    fn set_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.writer.set_read_timeout(timeout)
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
    /// Folders excluded through selective sync, fetched on first use
    excluded: Option<Vec<PathBuf>>,
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
            excluded: None,
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

    /// Folders excluded through selective sync. They no longer exist on disk.
    /// Empty when the daemon isn't reachable.
    pub fn excluded(&mut self) -> &[PathBuf] {
        if self.excluded.is_none() {
            let list = self
                .ask("get_ignore_set", &[])
                .and_then(|mut f| f.remove("ignore_set"))
                .unwrap_or_default();
            self.excluded = Some(list.into_iter().map(PathBuf::from).collect());
        }
        self.excluded.as_deref().unwrap_or_default()
    }

    pub fn is_excluded(&mut self, path: &Path) -> bool {
        self.excluded().iter().any(|p| p == path)
    }

    /// The nearest strict ancestor of `path` that is excluded through selective sync.
    pub fn excluded_ancestor(&mut self, path: &Path) -> Option<PathBuf> {
        let excluded = self.excluded();
        path.ancestors()
            .skip(1)
            .find(|a| excluded.iter().any(|p| p == a))
            .map(Path::to_path_buf)
    }

    /// Exclude folders through selective sync: Dropbox deletes the local copies but
    /// keeps them online. Returns the folders Dropbox reports as newly excluded.
    pub fn exclude(&mut self, folders: &[PathBuf]) -> Result<Vec<PathBuf>> {
        self.change_excluded("ignore_set_add", "ignored", folders)
    }

    /// Undo [`Dropbox::exclude`]: Dropbox downloads the folders again. Returns the
    /// folders Dropbox reports as no longer excluded.
    pub fn include(&mut self, folders: &[PathBuf]) -> Result<Vec<PathBuf>> {
        self.change_excluded("ignore_set_remove", "removed", folders)
    }

    fn change_excluded(
        &mut self,
        command: &str,
        reply_key: &str,
        folders: &[PathBuf],
    ) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for folder in folders {
            match folder.to_str() {
                Some(p) if !p.contains(['\t', '\n']) => paths.push(p),
                _ => bail!("Dropbox can't be sent the path {}", folder.display()),
            }
        }
        self.excluded = None;
        let daemon = self
            .daemon
            .as_mut()
            .ok_or_else(|| anyhow!("the Dropbox daemon isn't running or reachable"))?;
        // Dropbox answers once it has updated its sync state, which can take a while
        daemon.set_timeout(None)?;
        let reply = daemon.command(command, &[("paths", &paths.join("\t"))]);
        daemon.set_timeout(Some(Duration::from_secs(3)))?;
        let fields = reply
            .context("lost the connection to the Dropbox daemon")?
            .map_err(|message| anyhow!("Dropbox refused: {message}"))?;
        Ok(fields
            .get(reply_key)
            .into_iter()
            .flatten()
            .map(PathBuf::from)
            .collect())
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

/// Ignored files and folders inside `dir` (not below ignored folders). They exist
/// only on this computer, so excluding `dir` would lose them. Stops after `limit`.
pub fn ignored_within(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_ignored(&path) {
                found.push(path);
                if found.len() >= limit {
                    return found;
                }
            } else if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            }
        }
    }
    found
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
