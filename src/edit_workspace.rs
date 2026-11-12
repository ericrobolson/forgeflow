//! Scratch files live until their edit session finishes, then disappear together.
use std::path::PathBuf;
use std::sync::Mutex;

static ACTIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub struct EditWorkspace { pub path: PathBuf }
impl EditWorkspace {
    pub fn new(project: &crate::project::Project) -> Result<Self, String> {
        let relative = format!(".tmp/.edits/{}", uuid::Uuid::now_v7());
        let path = project.resolve(&relative)?;
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        ACTIVE.lock().map_err(|e| e.to_string())?.push(path.clone());
        Ok(Self { path })
    }
}
impl Drop for EditWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        if let Ok(mut active) = ACTIVE.lock() { active.retain(|p| p != &self.path); }
    }
}
pub fn cleanup_on_exit() {
    if let Ok(active) = ACTIVE.lock() { for path in active.iter() { let _ = std::fs::remove_dir_all(path); } }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_files_during_session_and_cleans_on_return_and_error() {
        let root = std::env::temp_dir().join(format!("forgeflow-workspace-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let project = crate::project::Project { root: root.clone() };
        let path;
        { let work = EditWorkspace::new(&project).unwrap(); path = work.path.clone();
          std::fs::write(path.join("crop.png"), "test").unwrap(); assert!(path.join("crop.png").exists()); }
        assert!(!path.exists());
        let fail = || -> Result<(), String> { let _work = EditWorkspace::new(&project)?; Err("cancelled".into()) };
        assert!(fail().is_err());
        assert_eq!(std::fs::read_dir(root.join(".tmp/.edits")).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
