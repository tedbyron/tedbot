//! Sled database operations.

use std::env;

use sled::{Config, Db, Mode};

/// Initialize a connection to a local `sled` database using environment variables for
/// configuration.
#[tracing::instrument]
pub fn init(file_name: &str) -> crate::Result<Db> {
    let mode = match env::var("TEDBOT_DB_MODE") {
        Ok(mode) => match mode.as_str() {
            "small" => Mode::LowSpace,
            "fast" => Mode::HighThroughput,
            _ => return Err(Box::from("INVALID TEDBOT_DB_MODE env var")),
        },
        Err(_) => return Err(Box::from("Missing TEDBOT_DB_MODE env var")),
    };
    let cache_size = match env::var("TEDBOT_DB_CACHE_SIZE") {
        Ok(size) => match size.parse::<u64>() {
            Ok(parsed) => parsed,
            Err(_) => return Err(Box::from("INVALID TEDBOT_DB_CACHE_SIZE env var")),
        },
        Err(_) => return Err(Box::from("Missing TEDBOT_DB_CACHE_SIZE env var")),
    };
    let (compression, factor) = if let Ok(level) = env::var("TEDBOT_DB_COMPRESSION") {
        match level.parse() {
            Ok(parsed @ 1..=22) => (true, parsed),
            _ => return Err(Box::from("INVALID TEDBOT_DB_COMPRESSION env var")),
        }
    } else {
        (false, 0)
    };

    let mut db_path = env::current_exe()?;

    db_path.pop();
    db_path.push(file_name);

    let cfg = Config::new()
        .path(db_path)
        .cache_capacity(cache_size)
        .use_compression(compression)
        .compression_factor(factor)
        .mode(mode);

    let db = cfg.open()?;
    if !db.was_recovered() {
        tracing::warn!(
            "Database {} was not recovered, creating new storage file",
            file_name
        );
    }

    Ok(db)
}
