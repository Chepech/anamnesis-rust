//! Which paths get indexed: watch-dir membership, file-type toggles, dotfiles and exclude globs.
//! Globs are compiled once per config change (TS recompiled minimatch per file).

use crate::config::Config;
use std::path::{Path, PathBuf};

struct Pattern {
    glob: globset::GlobMatcher,
    /// No `/`: matches any single path component, like minimatch `matchBase`.
    bare: bool,
}

impl Pattern {
    fn compile(p: &str) -> Option<Pattern> {
        let p = p.replace('\\', "/");
        let p = p.trim_matches('/');
        let glob = globset::GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .ok()?
            .compile_matcher();
        Some(Pattern {
            glob,
            bare: !p.contains('/'),
        })
    }

    fn matches(&self, rel: &Path) -> bool {
        if self.bare {
            rel.components().any(|c| self.glob.is_match(c.as_os_str()))
        } else {
            self.glob.is_match(rel)
        }
    }
}

fn compile_all(ps: &[String]) -> Vec<Pattern> {
    ps.iter().filter_map(|p| Pattern::compile(p)).collect()
}

pub struct Filter {
    roots: Vec<PathBuf>,
    exts: Vec<&'static str>,
    global: Vec<Pattern>,
    per_dir: Vec<(PathBuf, Vec<Pattern>)>,
}

impl Filter {
    pub fn new(cfg: &Config) -> Filter {
        let ft = &cfg.file_types;
        let exts = [
            ("md", ft.markdown),
            ("pdf", ft.pdf),
            ("docx", ft.docx),
            ("html", ft.html),
            ("htm", ft.html),
        ]
        .into_iter()
        .filter_map(|(e, on)| on.then_some(e))
        .collect();
        Filter {
            roots: cfg.watch_dirs.iter().map(PathBuf::from).collect(),
            exts,
            global: compile_all(&cfg.exclude_patterns),
            per_dir: cfg
                .dir_exclude_patterns
                .iter()
                .map(|(d, ps)| (PathBuf::from(d), compile_all(ps)))
                .collect(),
        }
    }

    /// True for a supported, enabled, non-excluded file inside a watch dir.
    pub fn is_indexable(&self, path: &Path) -> bool {
        let ext = crate::parsers::extension(path);
        self.exts.contains(&ext.as_str()) && self.allowed(path)
    }

    /// Cheap check for raw watcher events: an indexable file, or something that may be a
    /// directory (no extension). Deleted paths cannot be stat'ed, so this never touches disk.
    pub fn is_candidate(&self, path: &Path) -> bool {
        (path.extension().is_none() && self.allowed(path)) || self.is_indexable(path)
    }

    /// The watch dir that owns `path` (longest match), if any.
    pub fn root_of(&self, path: &Path) -> Option<PathBuf> {
        self.roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
            .cloned()
    }

    /// Inside a watch dir, no dot component, not excluded (extension not checked).
    pub fn allowed(&self, path: &Path) -> bool {
        let Some(root) = self.root_of(path) else {
            return false;
        };
        let rel = path.strip_prefix(&root).unwrap_or(path);
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            return false;
        }
        if self.global.iter().any(|p| p.matches(rel)) {
            return false;
        }
        !self.per_dir.iter().any(|(dir, ps)| {
            path.strip_prefix(dir)
                .is_ok_and(|r| ps.iter().any(|p| p.matches(r)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cfg() -> Config {
        Config {
            watch_dirs: vec!["/v".into(), "/w".into()],
            ..Config::default()
        }
    }

    fn ok(f: &Filter, p: &str) -> bool {
        f.is_indexable(Path::new(p))
    }

    #[test]
    fn supported_extensions_follow_file_type_toggles() {
        let f = Filter::new(&cfg());
        assert!(ok(&f, "/v/a.md") && ok(&f, "/v/a.pdf") && ok(&f, "/v/a.docx"));
        assert!(!ok(&f, "/v/a.html"), "html off by default");
        assert!(!ok(&f, "/v/a.txt") && !ok(&f, "/v/noext"));
        assert!(ok(&f, "/v/A.MD"), "case-insensitive extension");
        let mut c = cfg();
        c.file_types.html = true;
        c.file_types.pdf = false;
        let f = Filter::new(&c);
        assert!(ok(&f, "/v/a.html") && ok(&f, "/v/a.htm") && !ok(&f, "/v/a.pdf"));
    }

    #[test]
    fn paths_outside_watch_dirs_are_rejected() {
        let f = Filter::new(&cfg());
        assert!(!ok(&f, "/other/a.md"));
        assert!(!ok(&f, "/vault/a.md"), "prefix of a name is not membership");
        assert!(ok(&f, "/w/deep/er/a.md"));
    }

    #[test]
    fn dotfiles_and_dot_directories_are_skipped() {
        let f = Filter::new(&cfg());
        assert!(!ok(&f, "/v/.hidden.md"));
        assert!(!ok(&f, "/v/.trash/a.md"));
    }

    #[test]
    fn global_excludes_match_any_component_or_glob() {
        let mut c = cfg();
        c.exclude_patterns = vec![
            "node_modules".into(),
            "*.pdf".into(),
            "Archives/**".into(),
            "draft-*".into(),
        ];
        let f = Filter::new(&c);
        assert!(!ok(&f, "/v/x/node_modules/a.md"));
        assert!(!ok(&f, "/v/a.pdf"));
        assert!(!ok(&f, "/v/Archives/old/a.md"));
        assert!(ok(&f, "/v/Notes/Archives.md"));
        assert!(
            !ok(&f, "/v/sub/draft-1.md"),
            "matchBase: bare pattern hits basenames anywhere"
        );
        assert!(ok(&f, "/v/sub/final.md"));
    }

    #[test]
    fn per_dir_excludes_only_apply_inside_their_dir() {
        let mut c = cfg();
        c.dir_exclude_patterns = BTreeMap::from([("/v".to_string(), vec!["private".to_string()])]);
        let f = Filter::new(&c);
        assert!(!ok(&f, "/v/private/a.md"));
        assert!(ok(&f, "/w/private/a.md"));
    }

    #[test]
    fn invalid_globs_are_ignored_not_fatal() {
        let mut c = cfg();
        c.exclude_patterns = vec!["[".into(), "skip".into()];
        let f = Filter::new(&c);
        assert!(ok(&f, "/v/a.md"));
        assert!(!ok(&f, "/v/skip/a.md"));
    }

    #[test]
    fn candidates_include_possible_directories() {
        let f = Filter::new(&cfg());
        assert!(f.is_candidate(Path::new("/v/newdir")));
        assert!(f.is_candidate(Path::new("/v/a.md")));
        assert!(!f.is_candidate(Path::new("/v/a.tmp")));
        assert!(!f.is_candidate(Path::new("/v/.obsidian/workspace.json")));
        assert!(!f.is_candidate(Path::new("/elsewhere/dir")));
    }

    #[test]
    fn root_of_picks_longest_watch_dir() {
        let c = Config {
            watch_dirs: vec!["/v".into(), "/v/inner".into()],
            ..Config::default()
        };
        let f = Filter::new(&c);
        assert_eq!(
            f.root_of(Path::new("/v/inner/a.md")),
            Some(PathBuf::from("/v/inner"))
        );
        assert_eq!(f.root_of(Path::new("/v/a.md")), Some(PathBuf::from("/v")));
        assert_eq!(f.root_of(Path::new("/x/a.md")), None);
    }
}
