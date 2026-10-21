# di (dropboxignore)

Keep build artifacts and other junk out of Dropbox on Linux using `.gitignore`-style
rules, and see at a glance what Dropbox is actually syncing.

Put a `.dropboxignore` file in any folder inside your Dropbox. It uses `.gitignore`
syntax: deeper files override parent ones, and `!pattern` keeps something synced.
`di check` then sets Dropbox's `com.dropbox.ignored` attribute on every match.

```gitignore
# ~/Dropbox/Development/.dropboxignore
target/
node_modules/
*.tmp
```

## Commands

| Command | What it does |
| --- | --- |
| `check [PATH]` (alias `sync`) | Applies the rules: ignores every matching file and folder. Supports `--dry-run`, `--quiet`, `--summary` and `--unignore-unmatched`. |
| `tree [PATH]` (alias `ls`) | Shows a tree with tracking state and live sync status for each entry. Supports `-L <depth>`, `--dirs-only`, `--no-hidden` and `--no-sync`. |
| `status [PATH...]` (alias `st`) | Shows each path's tracking state, sync status, the rule that matches it, and its Dropbox folder. |
| `ignore PATH...` | Ignores paths right away. Asks first when a path is already in Dropbox, because ignoring deletes the online copy; `--yes` skips the question. Use `-r` to include everything inside, and `--dry-run` to preview. |
| `unignore PATH...` | Syncs paths again. Warns you if a rule will re-ignore them on the next `check`. |
| `exclude [PATH...]` | Makes folders online only through selective sync: Dropbox deletes them here and keeps them online. With no paths, lists excluded folders. Supports `--dry-run` and `--force`. |
| `include PATH...` | Downloads excluded folders again. |
| `tui [PATH]` | Opens an interactive browser for the tree, with live sync status, ignore/unignore, add-rule, editing and check. Press `?` for keys. |

```
$ di tree
⟳ ~/Dropbox/Development/rust/dropbox-ignore/  syncing
├── ✓ src/
│   ├── ✓ main.rs
│   └── ⟳ tree.rs     syncing
├── ⊘ target/     ignored target/ in .dropboxignore
├── ◌ notes.tmp   pending ignore *.tmp in ../.dropboxignore
└── ✓ Cargo.toml

2 folders, 4 files  ·  Dropbox: ⟳ Syncing 3 files
✓ 3 synced   ⟳ 2 syncing   ⊘ 1 ignored   ◌ 1 pending
```

`tree` never descends into untracked folders, because Dropbox ignores everything
inside them.

### Ignore vs. exclude

They do opposite things:

| | Copy on this computer | Copy in Dropbox |
| --- | --- | --- |
| `di ignore` | kept | removed, or never uploaded |
| `di exclude` | deleted | kept |

Ignoring something that's already in Dropbox deletes it from dropbox.com and your
other computers, so `di ignore` and the TUI ask first. `di check` doesn't ask, so
write rules only for things that shouldn't be online at all.

Use `ignore` for build output and other local junk. Use `exclude` to free space
here for folders you still want in Dropbox. Only folders can be excluded, because
the Linux client has no online-only files. `exclude` refuses folders that are still
syncing and folders that are ignored or hold ignored items, because those exist only
on this computer and would be deleted for good. `--force` skips those checks.
Excluded folders still show up in `tree` and `tui`, marked "online only".

### Hiding entries

`tree` and `tui` take `-I, --hide <GLOB>` to leave out entries by name. The entries
still sync; they just aren't shown. Repeat the flag or set it permanently:

```sh
di tree -I .git -I '*.swp'
export DI_HIDE='.git,node_modules'   # comma-separated, in your shell profile
```

`--no-hidden` hides every dotfile instead.

## TUI

`di tui` shows the same tree, with a details pane for the selected entry. Sync status
refreshes every 1.5 seconds.

| Key | Action |
| --- | --- |
| `↑↓` / `jk`, `→` / `l` / `enter`, `←` / `h`, `space` | move, open, close, toggle |
| `i` / `u` | ignore (asks first if it's in Dropbox) / unignore the selected entry |
| `x` | exclude the selected folder (online only), or download an excluded one again |
| `a` | add an anchored rule (e.g. `/build/`) to the folder's `.dropboxignore` and ignore the entry |
| `e` | edit the relevant `.dropboxignore` in `$VISUAL` / `$EDITOR`, then reload |
| `c` | preview everything `check` would change, then apply it with `y` |
| `/`, `n`, `N` | find among visible entries |
| `.`, `D`, `r` | toggle dotfiles, toggle details, refresh |
| `?`, `q` | help, quit |

The mouse works too: click to select, double-click to open, scroll to move.

### Legend

| Icon | Meaning |
| --- | --- |
| ✓ | tracked and up to date |
| ⟳ | tracked and syncing |
| ✗ | tracked, but Dropbox can't sync it |
| ☁ | excluded by selective sync |
| ● | tracked, but sync status is unknown (daemon not running, or `--no-sync`) |
| ⊘ grey | ignored by a rule |
| ⊘ magenta | ignored, but no rule matches it (ignored by hand) |
| ◌ | matches a rule but is not ignored yet; run `check` |
| ○ | not inside a Dropbox folder |

## Global options

- `--color auto|always|never`: color is on by default for terminals and respects `NO_COLOR`.
- `--icons unicode|nerd|ascii`: `nerd` adds file and folder icons if you use a Nerd Font.
  You can also set this with `DROPBOXIGNORE_ICONS`.

## How it works

- **Ignoring** sets the `user.com.dropbox.ignored` extended attribute, which the Linux
  Dropbox client reads.
- **Excluding** asks the daemon to change its selective sync list, the same as
  `dropbox exclude add`.
- **Sync status** comes from the running Dropbox daemon over `~/.dropbox/command_socket`,
  the same socket the official `dropbox filestatus` uses. If the daemon isn't running,
  everything still works except live sync status.
- **Dropbox folders** are found in `~/.dropbox/info.json`. Rules from parent folders, up
  to the Dropbox root, apply when you run a command on a subfolder.

## Running it periodically

`check` is quick and safe to repeat. A systemd user timer works well:

```ini
# ~/.config/systemd/user/dropboxignore.service
[Service]
Type=oneshot
ExecStart=%h/.cargo/bin/di check --quiet --color never %h/Dropbox

# ~/.config/systemd/user/dropboxignore.timer
[Timer]
OnCalendar=minutely
[Install]
WantedBy=timers.target
```

`check --summary` prints a single `+NEW (UNMATCHED) SIZE` line for status bars.

## Credits

This is a rewrite of [antbern/dropboxignore](https://github.com/antbern/dropboxignore) (MIT).
