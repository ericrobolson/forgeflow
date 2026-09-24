use std::path::PathBuf;

const PROJECT_FOLDER: &'static str = "_forgeflow";

pub struct Project {}
impl Project {
    pub fn exists() -> bool {
        let path: PathBuf = PROJECT_FOLDER.into();
        path.exists()
    }

    pub fn initialize(path: &PathBuf) -> Self {
        let path: PathBuf = PROJECT_FOLDER.into();
        if !path.exists() {
            std::fs::create_dir_all(&path).unwrap();
        }

        Self {}
    }
}
