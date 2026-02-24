use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

pub(crate) struct InodeMap {
    ino_to_path: HashMap<u64, PathBuf>,
    path_to_ino: HashMap<PathBuf, u64>,
}

impl InodeMap {
    pub fn new() -> Self {
        let mut map = Self {
            ino_to_path: HashMap::new(),
            path_to_ino: HashMap::new(),
        };
        // Inode 1 = root
        map.ino_to_path.insert(1, PathBuf::from("."));
        map.path_to_ino.insert(PathBuf::from("."), 1);
        map
    }

    pub fn insert(&mut self, path: PathBuf, real_ino: u64) -> u64 {
        if let Some(&existing) = self.path_to_ino.get(&path) {
            return existing;
        }
        self.ino_to_path.insert(real_ino, path.clone());
        self.path_to_ino.insert(path, real_ino);
        real_ino
    }

    pub fn get_path(&self, ino: u64) -> Option<&Path> {
        self.ino_to_path.get(&ino).map(|p| p.as_path())
    }

    pub fn remove_path(&mut self, path: &Path) {
        if let Some(ino) = self.path_to_ino.remove(path) {
            self.ino_to_path.remove(&ino);
        }
    }

    pub fn rename(&mut self, old: &Path, new_path: PathBuf) {
        if let Some(ino) = self.path_to_ino.remove(old) {
            self.ino_to_path.insert(ino, new_path.clone());
            self.path_to_ino.insert(new_path, ino);
        }
    }
}
