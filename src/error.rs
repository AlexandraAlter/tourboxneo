use std::{fmt, ops::Range};

#[derive(Debug, Clone)]
enum Location {
    Raw(Range<usize>),
    Exact { row: usize, col: usize, len: usize },
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Location::Raw(range) => write!(f, "bytes {} to {}", range.start, range.end),
            Location::Exact { row, col, len } => {
                write!(f, "row {}, col {}, for {} bytes", row, col, len)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigError {
    msg: &'static str,
    loc: Location,
}

impl ConfigError {
    pub fn new(msg: &'static str, range: Range<usize>) -> Self {
        Self {
            msg: msg,
            loc: Location::Raw(range),
        }
    }

    pub fn with_exact_loc(&self, config: &str) -> Self {
        match &self.loc {
            Location::Raw(range) => {
                let conf = &config[0..range.start];
                let mut row = 0;
                let mut col = 0;
                for b in conf.chars() {
                    if b == '\n' {
                        col = 0;
                        row += 1
                    } else {
                        col += 1;
                    }
                }
                let loc = Location::Exact {
                    row: row + 1,
                    col: col + 1,
                    len: range.len(),
                };
                ConfigError { msg: self.msg, loc }
            }
            Location::Exact { .. } => self.clone(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Config error, {}: {}", self.msg, self.loc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ConfigError;

    #[test]
    fn parses_exact_loc() {
        let error = ConfigError::new("", 10..14);
        let exact_error = error.with_exact_loc("test\ntest\ntest\ntest");
        if let Location::Exact { row, col, len } = exact_error.loc {
            assert_eq!(row, 3);
            assert_eq!(col, 1);
            assert_eq!(len, 4);
        } else {
            panic!("should contain exact location")
        }
    }
}
