use serde::Deserialize;
use std::error::Error;

pub type SarusResult<T> = std::result::Result<T, SarusError>;

pub const FILE_READ_ERROR: u64 = 2;
pub const TOML_DESERIALIZATION_ERROR: u64 = 3;
pub const CONFIG_VALIDATION_ERROR: u64 = 4;
pub const CONFIG_ACCESS_ERROR: u64 = 23;

#[derive(Debug, Clone, Deserialize)]
pub struct SarusError {
    pub code: u64,
    pub file_path: Option<String>,
    pub msg: String,
}

impl std::fmt::Display for SarusError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let fp = match &self.file_path {
            Some(p) => format!(" on {p}"),
            None => String::from(""),
        };
        write!(f, "Error {:03}{}: {}", self.code, fp, self.msg)
    }
}

impl Error for SarusError {}
