use std::{fmt, ops::Range};

#[derive(Debug, Clone)]
enum Location {
    Raw(Range<usize>),
    Exact { row: usize, col: usize, len: usize },
    None,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Location::Raw(range) => write!(f, "bytes {} to {}", range.start, range.end),
            Location::Exact { row, col, len } => {
                write!(f, "row {}, col {}, for {} bytes", row, col, len)
            }
            Location::None => {
                write!(f, "")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigError {
    msg: &'static str,
    path: Vec<String>,
}

impl ConfigError {
    pub fn new(msg: &'static str) -> Self {
        Self { msg: msg, path: Vec::new() }
    }

    pub fn add_path(&mut self, path_component: String) {
        self.path.push(path_component);
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Config error, {}: {}", self.msg, self.path.join("."))
    }
}
