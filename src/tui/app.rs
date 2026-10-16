//! TUI state and the actions keys trigger. Rendering lives in `view.rs`.

use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
    widgets::ListState,
};

use crate::{
    dropbox::{self, Dropbox},
    model::{Filter, Node, State, Target, list_children},
    plan::{Action, Plan},
    rules::{self, Rules, Verdict},
    ui::{self, Mark},
};

const FLASH_FOR: Duration = Duration::from_secs(5);

/// A folder's cached listing.
struct Listing {
    nodes: Vec<Node>,
    error: Option<String>,
}

pub enum Entry {
    Node(Node),
    /// An informational line inside a folder, like "empty" or a read error
    Note(String, Mark),
}

pub struct Row {
    pub entry: Entry,
    pub depth: usize,
    /// For each ancestor level below the root: was that ancestor the last sibling?
    pub ancestors_last: Vec<bool>,
    pub last: bool,
    pub expanded: bool,
    /// Width to pad names to, so labels line up among siblings
    pub column: usize,
}

impl Row {
    pub fn node(&self) -> Option<&Node> {
        match &self.entry {
            Entry::Node(node) => Some(node),
            Entry::Note(..) => None,
        }
    }
}

pub enum Modal {
    Help,
    Confirm {
        title: String,
        lines: Vec<String>,
        on_yes: Confirmed,
    },
    Plan {
        plan: Plan,
        scroll: usize,
    },
}

pub enum Confirmed {
    AddRule {
        file: PathBuf,
        pattern: String,
        target: PathBuf,
    },
}

pub struct Flash {
    pub text: String,
    pub mark: Mark,
    at: Instant,
}

pub struct App {
    pub root: Target,
    pub dropbox: Dropbox,
    pub filter: Filter,
    pub daemon_status: Option<String>,
    pub rows: Vec<Row>,
    pub list: ListState,
    pub modal: Option<Modal>,
    pub flash: Option<Flash>,
    /// Text being typed after `/`
    pub search: Option<String>,
    pub show_details: bool,
    pub quit: bool,
    /// Set by `e`; the event loop suspends the UI and opens an editor
    pub edit_request: Option<PathBuf>,
    /// Where the tree was drawn last frame, for mouse hit-testing
    pub tree_area: Rect,
    last_search: String,
    listings: HashMap<PathBuf, Listing>,
    rules: HashMap<PathBuf, Rules>,
    expanded: HashSet<PathBuf>,
    last_click: Option<(usize, Instant)>,
}

impl App {
    pub fn new(path: &Path, filter: Filter, query_daemon: bool) -> Result<Self> {
        let mut dropbox = Dropbox::discover(query_daemon);
        let root = Target::resolve(path, &mut dropbox)?;
        let daemon_status = dropbox.daemon_status();
        let mut app = Self {
            root,
            dropbox,
            filter,
            daemon_status,
            rows: Vec::new(),
            list: ListState::default().with_selected(Some(0)),
            modal: None,
            flash: None,
            search: None,
            show_details: true,
            quit: false,
            edit_request: None,
            tree_area: Rect::default(),
            last_search: String::new(),
            listings: HashMap::new(),
            rules: HashMap::new(),
            expanded: HashSet::new(),
            last_click: None,
        };
        app.rebuild();
        if !app.dropbox.daemon_connected() && query_daemon && app.root.state.is_tracked() {
            app.notify(
                Mark::Unknown,
                "Dropbox daemon not reachable, so sync status is unavailable",
            );
        }
        Ok(app)
    }

    // ----- tree model -----

    fn rules_for(&mut self, dir: &Path) -> Rules {
        if let Some(rules) = self.rules.get(dir) {
            return rules.clone();
        }
        let rules = match dir.parent() {
            Some(parent) if dir != self.root.path && dir.starts_with(&self.root.path) => {
                self.rules_for(parent).descend(dir)
            }
            _ => self.root.child_rules(),
        };
        self.rules.insert(dir.to_path_buf(), rules.clone());
        rules
    }

    fn load(&mut self, dir: &Path) {
        if self.listings.contains_key(dir) {
            return;
        }
        let rules = self.rules_for(dir);
        let listing = match list_children(dir, &rules, &mut self.dropbox, &self.filter) {
            Ok((nodes, _)) => Listing { nodes, error: None },
            Err(err) => Listing {
                nodes: Vec::new(),
                error: Some(format!("can't read folder: {err}")),
            },
        };
        self.listings.insert(dir.to_path_buf(), listing);
    }

    /// Recompute the visible rows from the expansion set, keeping the selection.
    pub fn rebuild(&mut self) {
        let selected = self.selected_node().map(|n| n.path.clone());
        let root = Node {
            name: ui::tilde(&self.root.path),
            path: self.root.path.clone(),
            kind: self.root.kind.clone(),
            state: self.root.state.clone(),
        };
        let explore = root.kind.is_dir() && root.state.is_tracked();
        self.rows = vec![Row {
            entry: Entry::Node(root),
            depth: 0,
            ancestors_last: Vec::new(),
            last: true,
            expanded: explore,
            column: 0,
        }];
        if explore {
            let path = self.root.path.clone();
            self.push_children(&path, 1, Vec::new());
        }
        let index = selected
            .and_then(|p| {
                self.rows
                    .iter()
                    .position(|r| r.node().is_some_and(|n| n.path == p))
            })
            .unwrap_or_else(|| self.selected().min(self.rows.len() - 1));
        self.list.select(Some(index));
    }

    fn push_children(&mut self, dir: &Path, depth: usize, ancestors_last: Vec<bool>) {
        self.load(dir);
        let listing = &self.listings[dir];
        if let Some(error) = &listing.error {
            let note = Entry::Note(error.clone(), Mark::Error);
            self.push_note(note, depth, ancestors_last);
            return;
        }
        if listing.nodes.is_empty() {
            self.push_note(
                Entry::Note("empty".into(), Mark::Unknown),
                depth,
                ancestors_last,
            );
            return;
        }
        let nodes = listing.nodes.clone();
        let column = nodes
            .iter()
            .map(|n| ui::entry_name_width(&n.name, &n.kind))
            .filter(|w| *w <= 40)
            .max()
            .unwrap_or(0);
        let count = nodes.len();
        for (i, node) in nodes.into_iter().enumerate() {
            let last = i + 1 == count;
            let expanded = self.expanded.contains(&node.path) && expandable(&node);
            let path = node.path.clone();
            self.rows.push(Row {
                entry: Entry::Node(node),
                depth,
                ancestors_last: ancestors_last.clone(),
                last,
                expanded,
                column,
            });
            if expanded {
                let mut next = ancestors_last.clone();
                next.push(last);
                self.push_children(&path, depth + 1, next);
            }
        }
    }

    fn push_note(&mut self, entry: Entry, depth: usize, ancestors_last: Vec<bool>) {
        self.rows.push(Row {
            entry,
            depth,
            ancestors_last,
            last: true,
            expanded: false,
            column: 0,
        });
    }

    /// Forget everything cached and re-read from disk.
    pub fn reload(&mut self) {
        self.listings.clear();
        self.rules.clear();
        match Target::resolve(&self.root.path, &mut self.dropbox) {
            Ok(root) => self.root = root,
            Err(err) => self.notify(Mark::Error, format!("{err:#}")),
        }
        self.daemon_status = self.dropbox.daemon_status();
        self.rebuild();
    }

    /// Re-read one folder (after changing something in it).
    fn reload_dir(&mut self, dir: &Path) {
        self.listings.remove(dir);
        self.rebuild();
    }

    /// Periodic refresh: daemon headline and the sync state of rows on screen.
    pub fn tick(&mut self) {
        self.daemon_status = self.dropbox.daemon_status();
        if self
            .flash
            .as_ref()
            .is_some_and(|f| f.at.elapsed() > FLASH_FOR)
        {
            self.flash = None;
        }

        let height = self.tree_area.height.saturating_sub(2) as usize;
        let start = self.list.offset();
        let mut structure_changed = false;
        for i in start..(start + height).min(self.rows.len()) {
            let Some(node) = self.rows[i].node() else {
                continue;
            };
            if node.path == self.root.path {
                continue;
            }
            let (path, is_dir, was_tracked) = (
                node.path.clone(),
                node.kind.is_dir(),
                node.state.is_tracked(),
            );
            let Some(parent) = path.parent().map(Path::to_path_buf) else {
                continue;
            };
            let rules = self.rules_for(&parent);
            let state = State::classify(&path, is_dir, &rules, &mut self.dropbox);
            structure_changed |= state.is_tracked() != was_tracked;

            if let Some(cached) = self
                .listings
                .get_mut(&parent)
                .and_then(|l| l.nodes.iter_mut().find(|n| n.path == path))
            {
                cached.state = state.clone();
            }
            if let Entry::Node(node) = &mut self.rows[i].entry {
                node.state = state;
            }
        }
        if structure_changed {
            self.rebuild();
        }
        self.collect_warnings();
    }

    fn collect_warnings(&mut self) {
        if let Some(warning) = ui::take_warnings().pop() {
            self.notify(Mark::Warning, warning);
        }
    }

    pub fn notify(&mut self, mark: Mark, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            mark,
            at: Instant::now(),
        });
    }

    // ----- selection -----

    pub fn selected(&self) -> usize {
        self.list.selected().unwrap_or(0)
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.rows.get(self.selected()).and_then(Row::node)
    }

    fn select(&mut self, index: usize) {
        self.list
            .select(Some(index.min(self.rows.len().saturating_sub(1))));
    }

    fn move_by(&mut self, delta: isize) {
        let index = self.selected().saturating_add_signed(delta);
        self.select(index);
    }

    fn page(&self) -> isize {
        (self.tree_area.height.saturating_sub(3) as isize).max(1)
    }

    /// The selected node, unless it is the root (which actions can't change).
    fn selected_child(&mut self) -> Option<Node> {
        let node = self.selected_node()?.clone();
        if node.path == self.root.path {
            self.notify(
                Mark::Warning,
                "That's the folder being viewed; select something inside it",
            );
            return None;
        }
        Some(node)
    }

    // ----- input -----

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.search.is_some() {
            self.on_search_key(key);
        } else if self.modal.is_some() {
            self.on_modal_key(key);
        } else {
            self.on_tree_key(key);
        }
        self.collect_warnings();
    }

    fn on_tree_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.move_by(self.page() / 2),
            KeyCode::Char('u') if ctrl => self.move_by(-self.page() / 2),
            KeyCode::PageDown => self.move_by(self.page()),
            KeyCode::PageUp => self.move_by(-self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.expand(),
            KeyCode::Left | KeyCode::Char('h') => self.collapse(),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('H') => self.collapse_all(),
            KeyCode::Char('i') => self.ignore_selected(),
            KeyCode::Char('u') => self.unignore_selected(),
            KeyCode::Char('a') => self.propose_rule(),
            KeyCode::Char('e') => self.edit_rules(),
            KeyCode::Char('c') => self.propose_check(),
            KeyCode::Char('r') => {
                self.reload();
                self.notify(Mark::Success, "Refreshed");
            }
            KeyCode::Char('.') => self.toggle_dotfiles(),
            KeyCode::Char('D') => self.show_details = !self.show_details,
            KeyCode::Char('/') => self.search = Some(String::new()),
            KeyCode::Char('n') => self.find(&self.last_search.clone(), 1, true),
            KeyCode::Char('N') => self.find(&self.last_search.clone(), -1, true),
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            _ => {}
        }
    }

    fn on_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match (modal, key.code) {
            (_, KeyCode::Esc | KeyCode::Char('q' | 'n')) => {}
            (Modal::Help, KeyCode::Char('?')) => {}
            (Modal::Help, _) => self.modal = Some(Modal::Help),
            (Modal::Confirm { on_yes, .. }, KeyCode::Char('y') | KeyCode::Enter) => {
                self.confirmed(on_yes)
            }
            (modal @ Modal::Confirm { .. }, _) => self.modal = Some(modal),
            (Modal::Plan { plan, .. }, KeyCode::Char('y') | KeyCode::Enter) => {
                self.apply_plan(plan)
            }
            (Modal::Plan { plan, scroll }, code) => {
                let scroll = match code {
                    KeyCode::Down | KeyCode::Char('j') => scroll + 1,
                    KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll + 10,
                    KeyCode::PageUp => scroll.saturating_sub(10),
                    _ => scroll,
                };
                let scroll = scroll.min(plan.changes.len().saturating_sub(1));
                self.modal = Some(Modal::Plan { plan, scroll });
            }
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let Some(query) = self.search.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.search = None;
                return;
            }
            KeyCode::Enter => {
                self.last_search = std::mem::take(query);
                self.search = None;
                return;
            }
            KeyCode::Backspace => {
                query.pop();
            }
            KeyCode::Char(c) => query.push(c),
            _ => return,
        }
        let query = query.clone();
        self.find(&query, 1, false);
    }

    /// Jump to the next visible row whose name contains `query` (case-insensitive).
    fn find(&mut self, query: &str, direction: isize, skip_current: bool) {
        if query.is_empty() {
            return;
        }
        let needle = query.to_lowercase();
        let len = self.rows.len() as isize;
        let start = self.selected() as isize + isize::from(skip_current) * direction;
        for step in 0..len {
            let i = (start + step * direction).rem_euclid(len) as usize;
            if self.rows[i]
                .node()
                .is_some_and(|n| n.name.to_lowercase().contains(&needle))
            {
                self.select(i);
                return;
            }
        }
        self.notify(
            Mark::Unknown,
            format!("No visible entry matches \"{query}\""),
        );
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.modal.is_some() || self.search.is_some() {
            return;
        }
        let area = self.tree_area;
        let inside = mouse.column >= area.x
            && mouse.column < area.x + area.width
            && mouse.row > area.y
            && mouse.row < area.y + area.height.saturating_sub(1);
        match mouse.kind {
            MouseEventKind::ScrollDown => self.move_by(3),
            MouseEventKind::ScrollUp => self.move_by(-3),
            MouseEventKind::Down(MouseButton::Left) if inside => {
                let index = self.list.offset() + (mouse.row - area.y - 1) as usize;
                if index >= self.rows.len() {
                    return;
                }
                let double = self
                    .last_click
                    .is_some_and(|(i, at)| i == index && at.elapsed() < Duration::from_millis(400));
                self.select(index);
                if double {
                    self.toggle();
                    self.last_click = None;
                } else {
                    self.last_click = Some((index, Instant::now()));
                }
            }
            _ => {}
        }
    }

    // ----- folding -----

    fn expand(&mut self) {
        let Some(row) = self.rows.get(self.selected()) else {
            return;
        };
        let Some(node) = row.node() else { return };
        if row.expanded {
            self.move_by(1);
        } else if expandable(node) {
            self.expanded.insert(node.path.clone());
            self.rebuild();
        } else if node.kind.is_dir() {
            self.notify(
                Mark::Ignored,
                "Untracked folder: Dropbox ignores everything inside it",
            );
        }
    }

    fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected()) else {
            return;
        };
        let is_root = row.depth == 0;
        if row.expanded && !is_root {
            if let Some(node) = row.node() {
                self.expanded.remove(&node.path.clone());
                self.rebuild();
            }
            return;
        }
        // jump to the parent row
        let depth = row.depth;
        if let Some(parent) = (0..self.selected())
            .rev()
            .find(|&i| self.rows[i].depth < depth)
        {
            self.select(parent);
        }
    }

    fn toggle(&mut self) {
        match self.rows.get(self.selected()) {
            Some(row) if row.expanded => self.collapse(),
            Some(_) => self.expand(),
            None => {}
        }
    }

    fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild();
        self.select(0);
    }

    fn toggle_dotfiles(&mut self) {
        self.filter.show_dotfiles = !self.filter.show_dotfiles;
        self.listings.clear();
        self.rebuild();
        let text = if self.filter.show_dotfiles {
            "Showing dotfiles"
        } else {
            "Hiding dotfiles"
        };
        self.notify(Mark::Success, text);
    }

    // ----- actions -----

    fn ignore_selected(&mut self) {
        let Some(node) = self.selected_child() else {
            return;
        };
        if !node.state.is_tracked() {
            self.notify(Mark::Ignored, format!("{} is already ignored", node.name));
            return;
        }
        match dropbox::set_ignored(&node.path, true) {
            Ok(()) => {
                let hint = if matches!(node.state, State::Pending(_)) {
                    ""
                } else {
                    " (press a to add a rule so it stays ignored)"
                };
                self.notify(Mark::Ignored, format!("Ignored {}{hint}", node.name));
                self.reload_parent(&node.path);
            }
            Err(err) => self.notify(Mark::Error, format!("{err:#}")),
        }
    }

    fn unignore_selected(&mut self) {
        let Some(node) = self.selected_child() else {
            return;
        };
        if !matches!(node.state, State::Ignored(_)) {
            self.notify(Mark::Tracked, format!("{} is not ignored", node.name));
            return;
        }
        match dropbox::set_ignored(&node.path, false) {
            Ok(()) => {
                let text = match &node.state {
                    State::Ignored(Some(rule)) => format!(
                        "Unignored {}, but rule {} will ignore it again on the next check",
                        node.name, rule.pattern
                    ),
                    _ => format!("Unignored {}; Dropbox will sync it again", node.name),
                };
                let mark = if text.contains("rule") {
                    Mark::Warning
                } else {
                    Mark::Restored
                };
                self.notify(mark, text);
                self.reload_parent(&node.path);
            }
            Err(err) => self.notify(Mark::Error, format!("{err:#}")),
        }
    }

    fn reload_parent(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.reload_dir(parent);
        }
    }

    /// Offer to add an anchored rule for the selection to its folder's `.dropboxignore`.
    fn propose_rule(&mut self) {
        let Some(node) = self.selected_child() else {
            return;
        };
        let Some(parent) = node.path.parent() else {
            return;
        };
        let pattern = format!(
            "/{}{}",
            escape_glob(&node.name),
            if node.kind.is_dir() { "/" } else { "" }
        );
        let file = parent.join(rules::FILE_NAME);
        let mut lines = vec![
            format!("Append  {pattern}"),
            format!("to      {}", ui::pretty_path(&file, Some(&self.root.path))),
        ];
        if !node.state.is_tracked() {
            lines.push(String::new());
            lines.push("It is already ignored; the rule keeps it that way.".into());
        } else {
            lines.push(String::new());
            lines.push("and ignore it right away.".into());
        }
        self.modal = Some(Modal::Confirm {
            title: "Add ignore rule".into(),
            lines,
            on_yes: Confirmed::AddRule {
                file,
                pattern,
                target: node.path,
            },
        });
    }

    fn confirmed(&mut self, action: Confirmed) {
        match action {
            Confirmed::AddRule {
                file,
                pattern,
                target,
            } => match append_rule(&file, &pattern) {
                Ok(()) => {
                    let result = if dropbox::is_ignored(&target) {
                        Ok(())
                    } else {
                        dropbox::set_ignored(&target, true)
                    };
                    match result {
                        Ok(()) => {
                            self.notify(Mark::Success, format!("Added {pattern} and ignored it"))
                        }
                        Err(err) => self.notify(Mark::Error, format!("Rule added, but {err:#}")),
                    }
                    self.reload();
                }
                Err(err) => self.notify(Mark::Error, format!("{err:#}")),
            },
        }
    }

    fn edit_rules(&mut self) {
        let Some(node) = self.selected_node() else {
            return;
        };
        let dir = if node.kind.is_dir() && node.state.is_tracked() {
            node.path.clone()
        } else {
            node.path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| node.path.clone())
        };
        self.edit_request = Some(dir.join(rules::FILE_NAME));
    }

    fn propose_check(&mut self) {
        if !(self.root.kind.is_dir() && self.root.state.is_tracked()) {
            self.notify(
                Mark::Ignored,
                "This folder isn't synced, so there is nothing to check",
            );
            return;
        }
        let plan = Plan::scan(&self.root.path, self.root.child_rules(), false);
        if plan.changes.is_empty() {
            self.notify(
                Mark::Success,
                "Everything already matches your .dropboxignore rules",
            );
        } else {
            self.modal = Some(Modal::Plan { plan, scroll: 0 });
        }
    }

    fn apply_plan(&mut self, plan: Plan) {
        if plan.is_noop() {
            return;
        }
        let (mut done, mut failed) = (0, 0);
        for change in plan.changes.iter().filter(|c| c.action == Action::Ignore) {
            match change.apply() {
                Ok(()) => done += 1,
                Err(_) => failed += 1,
            }
        }
        self.reload();
        if failed > 0 {
            self.notify(
                Mark::Error,
                format!("Ignored {done}, but {failed} could not be changed"),
            );
        } else {
            self.notify(
                Mark::Success,
                format!("Ignored {}", ui::plural(done, "item", "items")),
            );
        }
    }
}

/// Whether a node is a folder the tree may open.
pub fn expandable(node: &Node) -> bool {
    node.kind.is_dir() && node.state.is_tracked()
}

fn escape_glob(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        if "*?[]\\".contains(c) || (i == 0 && "!#".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn append_rule(file: &Path, pattern: &str) -> Result<()> {
    let existing = std::fs::read_to_string(file).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == pattern) {
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .with_context(|| format!("can't write {}", ui::tilde(file)))?;
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    writeln!(f, "{separator}{pattern}")?;
    Ok(())
}

/// Used by the details pane to explain what a rule match means.
pub fn verdict_for(app: &mut App, node: &Node) -> Verdict {
    match node.path.parent() {
        Some(parent) if node.path != app.root.path => {
            let rules = app.rules_for(parent);
            rules.check(&node.path, node.kind.is_dir())
        }
        _ => app.root.own_rules().check(&node.path, node.kind.is_dir()),
    }
}
