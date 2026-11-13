use std::net::IpAddr;
use std::path::PathBuf;

/// Shortest nickname the server accepts (it answers `ParameterInvalidSize`).
pub const NICKNAME_MIN_CHARS: usize = 3;
/// Longest nickname the server accepts. Measured in characters, not bytes.
pub const NICKNAME_MAX_CHARS: usize = 30;

/// Why a nickname cannot be used. The server trims it and then counts
/// characters, so the check does the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NicknameError {
    TooShort,
    TooLong,
}

impl std::fmt::Display for NicknameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a nickname needs {NICKNAME_MIN_CHARS} to {NICKNAME_MAX_CHARS} characters"
        )
    }
}

impl std::error::Error for NicknameError {}

/// Trims `raw` and checks its length against the server's limits.
pub fn check_nickname(raw: &str) -> Result<&str, NicknameError> {
    let name = raw.trim();
    match name.chars().count() {
        n if n < NICKNAME_MIN_CHARS => Err(NicknameError::TooShort),
        n if n > NICKNAME_MAX_CHARS => Err(NicknameError::TooLong),
        _ => Ok(name),
    }
}

/// Everything needed to open one server connection.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `host`, `host:port` or a `ts3server://` URL. Port defaults to 9987.
    pub address: String,
    pub nickname: String,
    /// Channel name or numeric id to join right after connecting.
    pub channel: Option<String>,
    pub channel_password: Option<String>,
    pub server_password: Option<String>,
    /// Local IP to bind the UDP socket to. Useful to run several clients on one
    /// machine (the server rate-limits and can ban a single source IP).
    pub local_ip: Option<IpAddr>,
    /// Where the client identity (P-256 key) is persisted.
    pub identity_file: PathBuf,
    /// Dump every command/packet to the log (very noisy, for debugging).
    pub log_commands: bool,
}

impl ClientConfig {
    pub fn new(address: impl Into<String>, nickname: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            nickname: nickname.into(),
            channel: None,
            channel_password: None,
            server_password: None,
            local_ip: None,
            identity_file: default_config_dir().join("identity.json"),
            log_commands: false,
        }
    }

    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    pub fn channel_password(mut self, pwd: impl Into<String>) -> Self {
        self.channel_password = Some(pwd.into());
        self
    }

    pub fn server_password(mut self, pwd: impl Into<String>) -> Self {
        self.server_password = Some(pwd.into());
        self
    }

    pub fn identity_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.identity_file = path.into();
        self
    }

    pub fn local_ip(mut self, ip: impl Into<IpAddr>) -> Self {
        self.local_ip = Some(ip.into());
        self
    }

    pub fn log_commands(mut self, enabled: bool) -> Self {
        self.log_commands = enabled;
        self
    }
}

/// `%APPDATA%\ts6client` on Windows, `$XDG_CONFIG_HOME/ts6client` (or
/// `~/.config/ts6client`) elsewhere.
pub fn default_config_dir() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("ts6client");
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("ts6client");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("ts6client")
}

#[cfg(test)]
mod tests {
    #[test]
    fn nickname_limits_match_the_server() {
        use super::{check_nickname, NicknameError};
        assert_eq!(check_nickname("a"), Err(NicknameError::TooShort));
        assert_eq!(check_nickname("  ab  "), Err(NicknameError::TooShort));
        assert_eq!(check_nickname(" abc "), Ok("abc"));
        assert_eq!(check_nickname(&"é".repeat(30)).map(str::len), Ok(60));
        assert_eq!(check_nickname(&"x".repeat(31)), Err(NicknameError::TooLong));
    }

    use super::*;

    #[test]
    fn builders_set_the_expected_fields() {
        let cfg = ClientConfig::new("127.0.0.1:9987", "tester")
            .channel("Lobby")
            .channel_password("channel")
            .server_password("server")
            .local_ip(std::net::IpAddr::from([127, 0, 0, 2]))
            .log_commands(true);

        assert_eq!(cfg.address, "127.0.0.1:9987");
        assert_eq!(cfg.nickname, "tester");
        assert_eq!(cfg.channel.as_deref(), Some("Lobby"));
        assert_eq!(cfg.channel_password.as_deref(), Some("channel"));
        assert_eq!(cfg.server_password.as_deref(), Some("server"));
        assert_eq!(cfg.local_ip, Some(std::net::IpAddr::from([127, 0, 0, 2])));
        assert!(cfg.log_commands);
    }

    #[test]
    fn defaults_are_empty_and_store_the_identity_in_the_config_dir() {
        let cfg = ClientConfig::new("example.org", "nick");

        assert!(cfg.channel.is_none());
        assert!(cfg.channel_password.is_none());
        assert!(cfg.server_password.is_none());
        assert!(cfg.local_ip.is_none());
        assert!(!cfg.log_commands);
        assert!(cfg.identity_file.ends_with("identity.json"));
        assert!(cfg
            .identity_file
            .parent()
            .is_some_and(|dir| dir.ends_with("ts6client")));
    }
}
