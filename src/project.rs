use std::path::{Component, Path, PathBuf};

const PROJECT_FOLDER: &'static str = "_forgeflow";
pub const SESSIONS_FOLDER: &str = "sessions";
pub const REGISTERS_FILE: &str = "registers.json";
pub const SERVER_LOG_FILE: &str = "llama-server.log";
/// Per-machine files in `_forgeflow` that stay out of git.
const IGNORED_FILES: [&str; 3] = [crate::config::CONFIG_FILE, REGISTERS_FILE, SERVER_LOG_FILE];

pub struct Project {
    /// The folder containing `_forgeflow`; file words are confined to it.
    pub root: PathBuf,
}
impl Project {
    pub fn exists() -> bool {
        let path: PathBuf = PROJECT_FOLDER.into();
        path.exists()
    }

    /// Creates the project folders and their `.gitignore` files. Safe to run on every
    /// launch: existing projects gain anything missing, and nothing is overwritten.
    pub fn initialize(path: &PathBuf) -> std::io::Result<Self> {
        let project = Self { root: path.clone() };
        let folder = project.folder();
        std::fs::create_dir_all(&folder)?;
        ignore_entries(&folder, &IGNORED_FILES)?;
        Self::create_ignored_folder(&project.sessions_folder())?;
        Ok(project)
    }

    /// The project's `_forgeflow` folder.
    pub fn folder(&self) -> PathBuf {
        self.root.join(PROJECT_FOLDER)
    }

    pub fn sessions_folder(&self) -> PathBuf {
        self.folder().join(SESSIONS_FOLDER)
    }

    /// Creates `folder` with a `.gitignore` of `*`, keeping generated files out of git.
    pub fn create_ignored_folder(folder: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(folder)?;
        ignore_entries(folder, &["*"])
    }

    /// Resolves a relative path inside the project root, refusing anything that escapes it.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, String> {
        // Models often emit a stray space before a path; no real path here starts or ends with one.
        let relative = Path::new(relative.trim());
        if relative.is_absolute() {
            return Err(format!(
                "`{}` is absolute; use a path relative to the project root",
                relative.display()
            ));
        }
        let mut depth = 0i32;
        for component in relative.components() {
            match component {
                Component::ParentDir => depth -= 1,
                Component::Normal(_) => depth += 1,
                _ => {}
            }
            if depth < 0 {
                return Err(format!(
                    "`{}` leaves the project root",
                    relative.display()
                ));
            }
        }
        let resolved = self.root.join(relative);
        // Canonicalize the deepest existing ancestor as the final component may not
        // exist yet (for example, a write below an escaping symlink).
        let root = self.root.canonicalize().map_err(|e| format!("project root: {e}"))?;
        let mut ancestor = resolved.as_path();
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or_else(|| format!("invalid path `{}`", relative.display()))?;
        }
        let real = ancestor.canonicalize().map_err(|e| e.to_string())?;
        if !real.starts_with(&root) {
            return Err(format!("`{}` resolves outside the project root", relative.display()));
        }
        // If the target exists, validate its resolved location too (including a final symlink).
        if resolved.exists() && !resolved.canonicalize().map_err(|e| e.to_string())?.starts_with(&root) {
            return Err(format!("`{}` resolves outside the project root", relative.display()));
        }
        if resolved.exists() {
            return resolved.canonicalize().map_err(|e| e.to_string());
        }
        // Return a path rooted at the canonical ancestor. This prevents a caller
        // from swapping an in-project symlinked parent after this check.
        let suffix = resolved.strip_prefix(ancestor).map_err(|e| e.to_string())?;
        if suffix.as_os_str().is_empty() { Ok(real) } else { Ok(real.join(suffix)) }
    }
}

/// Adds each entry to `folder/.gitignore` unless a line already lists it, keeping any
/// lines added by hand.
fn ignore_entries(folder: &Path, entries: &[&str]) -> std::io::Result<()> {
    let path = folder.join(".gitignore");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let missing: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|entry| !text.lines().any(|line| line.trim() == *entry))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for entry in missing {
        text.push_str(entry);
        text.push('\n');
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_creates_folders_and_ignores_local_files() {
        let root = std::env::temp_dir().join(format!("forgeflow-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let project = Project::initialize(&root).unwrap();
        let read = |path: PathBuf| std::fs::read_to_string(path).unwrap();
        assert_eq!(
            read(project.folder().join(".gitignore")),
            "config.json\nregisters.json\nllama-server.log\n"
        );
        assert_eq!(read(project.sessions_folder().join(".gitignore")), "*\n");

        // Rerunning adds only what is missing and keeps lines added by hand.
        std::fs::write(project.folder().join(".gitignore"), "config.json\nnotes.txt").unwrap();
        Project::initialize(&root).unwrap();
        assert_eq!(
            read(project.folder().join(".gitignore")),
            "config.json\nnotes.txt\nregisters.json\nllama-server.log\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_stays_inside_the_root() {
        let project = Project {
            root: std::env::temp_dir(),
        };
        let cases = [
            ("src/main.rs", true),
            (" src/main.rs ", true),
            (".", true),
            ("a/../b", true),
            ("../etc/passwd", false),
            ("a/../../b", false),
            ("/etc/passwd", false),
        ];
        for (path, ok) in cases {
            assert_eq!(project.resolve(path).is_ok(), ok, "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_missing_children_beneath_escaping_symlinks() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("forgeflow-symlink-{}", std::process::id()));
        let root = base.join("project");
        let outside = base.join("outside");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        let project = Project { root: root.clone() };
        assert!(project.resolve("escape/new.txt").is_err());
        assert!(project.resolve("escape").is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
