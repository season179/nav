use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub cwd: String,
}

#[derive(Debug, Clone)]
pub struct Harness {
    name: &'static str,
    version: &'static str,
}

impl Harness {
    pub fn new(name: &'static str, version: &'static str) -> Self {
        Self { name, version }
    }

    pub fn hello(&self, cwd: Option<String>) -> BackendInfo {
        BackendInfo {
            name: self.name,
            version: self.version,
            cwd: cwd.unwrap_or_else(current_dir),
        }
    }
}

fn current_dir() -> String {
    env::current_dir().map_or_else(|_| ".".to_string(), |path| path.display().to_string())
}
