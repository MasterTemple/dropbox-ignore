//! `.dropboxignore` rule handling with `.gitignore` semantics: rules in deeper
//! folders take precedence, and `!pattern` re-includes something a parent excluded.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::{Match, gitignore::Gitignore};

use crate::ui;

pub const FILE_NAME: &str = ".dropboxignore";

/// The rule that decided a path's fate.
#[derive(Debug, Clone)]
pub struct Rule {
    /// The pattern as written, including a leading `!` for re-includes
    pub pattern: String,
    /// The `.dropboxignore` file it came from
    pub source: PathBuf,
}

impl Rule {
    /// `pattern in path/to/.dropboxignore`, with the source relative to `base`.
    pub fn describe(&self, base: Option<&Path>) -> String {
        format!(
            "{} {}",
            ui::paint(&self.pattern, owo_colors::Style::new().italic()),
            ui::dim(format!("in {}", ui::pretty_path(&self.source, base)))
        )
    }
}

#[derive(Debug, Clone)]
pub enum Verdict {
    /// No rule mentions the path
    Unmatched,
    /// A rule says the path should be ignored
    Ignore(Rule),
    /// A `!pattern` rule says the path must stay synced
    Keep(Rule),
}

impl Verdict {
    pub fn should_ignore(&self) -> bool {
        matches!(self, Verdict::Ignore(_))
    }

    pub fn rule(&self) -> Option<&Rule> {
        match self {
            Verdict::Unmatched => None,
            Verdict::Ignore(rule) | Verdict::Keep(rule) => Some(rule),
        }
    }
}

/// The stack of `.dropboxignore` files that apply inside one directory.
#[derive(Clone, Default)]
pub struct Rules {
    layers: Vec<Arc<Gitignore>>,
}

impl Rules {
    /// Rules that apply to the children of `dir`, loading every `.dropboxignore`
    /// from `top` (usually the Dropbox folder) down to `dir` itself.
    pub fn for_dir(dir: &Path, top: Option<&Path>) -> Self {
        let top = top.filter(|t| dir.starts_with(t)).unwrap_or(dir);
        let mut chain: Vec<&Path> = dir.ancestors().take_while(|p| p.starts_with(top)).collect();
        chain.reverse();
        chain
            .into_iter()
            .fold(Rules::default(), |rules, d| rules.descend(d))
    }

    /// The rules for the children of `dir`, given these are the rules of its parent.
    pub fn descend(&self, dir: &Path) -> Self {
        let file = dir.join(FILE_NAME);
        if !file.is_file() {
            return self.clone();
        }
        let (gitignore, error) = Gitignore::new(&file);
        if let Some(error) = error {
            ui::warn(format!("{}: {error}", ui::tilde(&file)));
        }
        let mut next = self.clone();
        if !gitignore.is_empty() {
            next.layers.push(Arc::new(gitignore));
        }
        next
    }

    pub fn check(&self, path: &Path, is_dir: bool) -> Verdict {
        for layer in self.layers.iter().rev() {
            let rule = |glob: &ignore::gitignore::Glob| Rule {
                pattern: glob.original().to_string(),
                source: glob
                    .from()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| layer.path().join(FILE_NAME)),
            };
            match layer.matched(path, is_dir) {
                Match::None => continue,
                Match::Ignore(glob) => return Verdict::Ignore(rule(glob)),
                Match::Whitelist(glob) => return Verdict::Keep(rule(glob)),
            }
        }
        Verdict::Unmatched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deeper_rules_win() {
        let dir = std::env::temp_dir().join(format!("dropboxignore-test-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join(FILE_NAME), "*.log\nbuild/\n").unwrap();
        std::fs::write(sub.join(FILE_NAME), "!keep.log\n").unwrap();

        let top = Rules::for_dir(&dir, None);
        assert!(top.check(&dir.join("a.log"), false).should_ignore());
        assert!(top.check(&dir.join("build"), true).should_ignore());
        assert!(!top.check(&dir.join("build"), false).should_ignore());

        let inner = top.descend(&sub);
        assert!(inner.check(&sub.join("x.log"), false).should_ignore());
        assert!(matches!(
            inner.check(&sub.join("keep.log"), false),
            Verdict::Keep(_)
        ));

        let from_top = Rules::for_dir(&sub, Some(&dir));
        assert!(matches!(
            from_top.check(&sub.join("keep.log"), false),
            Verdict::Keep(_)
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
