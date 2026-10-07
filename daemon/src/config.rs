//! Daemon policy, independent of transport, platform capture and storage.
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_RETENTION_DAYS: u64 = 7;
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

pub struct Config {
    pub root: PathBuf,
    pub budget: i64,
    pub retention_seconds: Option<i64>,
    pub capture: bool,
}

impl Config {
    pub fn load() -> std::io::Result<Self> {
        let root = std::env::var_os("RLDYOUR_CLIPBOARD_HOME")
            .map(PathBuf::from)
            .map_or_else(crate::store::default_root, Ok)?;
        Ok(Self {
            root,
            budget: parse_budget(std::env::var("RLDYOUR_CLIPBOARD_BUDGET").ok().as_deref()),
            retention_seconds: parse_retention(
                std::env::var("RLDYOUR_CLIPBOARD_RETENTION_DAYS")
                    .ok()
                    .as_deref(),
            ),
            capture: std::env::var_os("RLDYOUR_CLIPBOARD_CAPTURE").is_none_or(|value| value != "0"),
        })
    }
}

fn parse_budget(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .map(|value| if value == 0 { i64::MAX } else { value })
        .unwrap_or(crate::store::DEFAULT_BUDGET)
}

fn parse_retention(value: Option<&str>) -> Option<i64> {
    let days = value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|days| *days <= 36_500)
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    (days != 0).then_some(days as i64 * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_invalid_policy_do_not_accidentally_purge_everything() {
        assert_eq!(parse_retention(None), Some(604_800));
        assert_eq!(parse_retention(Some("-1")), Some(604_800));
        assert_eq!(parse_retention(Some("bad")), Some(604_800));
        assert_eq!(parse_retention(Some("0")), None);
        assert_eq!(parse_budget(Some("0")), i64::MAX);
        assert_eq!(parse_budget(Some("-1")), crate::store::DEFAULT_BUDGET);
    }
}
