//! Deserialize TOML config file.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;

/// Representation of a `tedbot-rs` config file.
#[derive(Debug, serde::Deserialize)]
pub struct Config<'cfg> {
    #[serde(borrow)]
    pub token: &'cfg str,
}

/// Load a config file from the current directory into a buffer and return the deserialized
/// [`Config`]. The buffer's capacity should be greater than or equal to the file's contents to
/// avoid reallocations.
#[tracing::instrument(skip(buf))]
pub fn load<'a>(path: &OsStr, buf: &'a mut String) -> Result<Config<'a>, crate::Error> {
    let mut file = File::open(path)?;
    file.read_to_string(buf)?;
    let cfg: Config<'a> = toml::from_str(buf)?;

    tracing::trace!(?cfg);
    tracing::info!("{:?} loaded", path);

    Ok(cfg)
}
