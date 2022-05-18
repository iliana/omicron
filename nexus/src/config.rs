// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Interfaces for parsing configuration files and working with a nexus server
//! configuration

use crate::db;
use anyhow::anyhow;
use dropshot::ConfigDropshot;
use dropshot::ConfigLogging;
use serde::Deserialize;
use serde::Serialize;
use serde_with::DeserializeFromStr;
use serde_with::SerializeDisplay;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

// By design, we require that all config properties be specified (i.e., we don't
// use `serde(default)`).

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AuthnConfig {
    /// allowed authentication schemes for external HTTP server
    pub schemes_external: Vec<SchemeName>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ConsoleConfig {
    pub static_dir: PathBuf,
    /// how long the browser can cache static assets
    pub cache_control_max_age_minutes: u32,
    /// how long a session can be idle before expiring
    pub session_idle_timeout_minutes: u32,
    /// how long a session can exist before expiring
    pub session_absolute_timeout_minutes: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UpdatesConfig {
    /// Trusted root.json role for the TUF updates repository.
    pub trusted_root: PathBuf,
    /// Default base URL for the TUF repository.
    pub default_base_url: String,
}

/// Configuration for the timeseries database.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TimeseriesDbConfig {
    pub address: SocketAddr,
}

// A deserializable type that does no validation on the tunable parameters.
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct UnvalidatedTunables {
    max_vpc_ipv4_subnet_prefix: u8,
}

/// Tunable configuration parameters, intended for use in test environments or
/// other situations in which experimentation / tuning is valuable.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(try_from = "UnvalidatedTunables")]
pub struct Tunables {
    /// The maximum prefix size supported for VPC Subnet IPv4 subnetworks.
    ///
    /// Note that this is the maximum _prefix_ size, which sets the minimum size
    /// of the subnet.
    pub max_vpc_ipv4_subnet_prefix: u8,
}

// Convert from the unvalidated tunables, verifying each parameter as needed.
impl TryFrom<UnvalidatedTunables> for Tunables {
    type Error = InvalidTunable;

    fn try_from(unvalidated: UnvalidatedTunables) -> Result<Self, Self::Error> {
        Tunables::validate_ipv4_prefix(unvalidated.max_vpc_ipv4_subnet_prefix)?;
        Ok(Tunables {
            max_vpc_ipv4_subnet_prefix: unvalidated.max_vpc_ipv4_subnet_prefix,
        })
    }
}

impl Tunables {
    fn validate_ipv4_prefix(prefix: u8) -> Result<(), InvalidTunable> {
        let absolute_max: u8 = 32_u8.checked_sub(
            // Always need space for the reserved Oxide addresses, including the
            // broadcast address at the end of the subnet.
            ((crate::defaults::NUM_INITIAL_RESERVED_IP_ADDRESSES + 1) as f32)
                .log2() // Subnet size to bit prefix.
                .ceil() // Round up to a whole number of bits.
                as u8
            ).expect("Invalid absolute maximum IPv4 subnet prefix");
        if prefix >= crate::defaults::MIN_VPC_IPV4_SUBNET_PREFIX
            && prefix <= absolute_max
        {
            Ok(())
        } else {
            Err(InvalidTunable {
                tunable: String::from("max_vpc_ipv4_subnet_prefix"),
                message: format!(
                    "IPv4 subnet prefix must be in the range [0, {}], found: {}",
                    absolute_max,
                    prefix,
                ),
            })
        }
    }
}

/// The maximum prefix size by default.
///
/// There are 6 Oxide reserved IP addresses, 5 at the beginning for DNS and the
/// like, and the broadcast address at the end of the subnet. This size provides
/// room for 2 ** 6 - 6 = 58 IP addresses, which seems like a reasonable size
/// for the smallest subnet that's still useful in many contexts.
pub const MAX_VPC_IPV4_SUBNET_PREFIX: u8 = 26;

impl Default for Tunables {
    fn default() -> Self {
        Tunables { max_vpc_ipv4_subnet_prefix: MAX_VPC_IPV4_SUBNET_PREFIX }
    }
}

/// Configuration for a nexus server
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Config {
    /// Dropshot configuration for external API server
    pub dropshot_external: ConfigDropshot,
    /// Dropshot configuration for internal API server
    pub dropshot_internal: ConfigDropshot,
    /// Identifier for this instance of Nexus
    pub id: uuid::Uuid,
    /// Console-related tunables
    pub console: ConsoleConfig,
    /// Server-wide logging configuration.
    pub log: ConfigLogging,
    /// Database parameters
    pub database: db::Config,
    /// Authentication-related configuration
    pub authn: AuthnConfig,
    /// Timeseries database configuration.
    pub timeseries_db: TimeseriesDbConfig,
    /// Updates-related configuration. Updates APIs return 400 Bad Request when this is
    /// unconfigured.
    #[serde(default)]
    pub updates: Option<UpdatesConfig>,
    /// Tunable configuration for testing and experimentation
    #[serde(default)]
    pub tunables: Tunables,
}

#[derive(Debug)]
pub struct InvalidTunable {
    tunable: String,
    message: String,
}

impl std::fmt::Display for InvalidTunable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "invalid \"{}\": \"{}\"", self.tunable, self.message)
    }
}

impl std::error::Error for InvalidTunable {}

#[derive(Debug)]
pub struct LoadError {
    path: PathBuf,
    kind: LoadErrorKind,
}
#[derive(Debug)]
pub enum LoadErrorKind {
    Io(std::io::Error),
    Parse(toml::de::Error),
    InvalidTunable(InvalidTunable),
}

impl From<(PathBuf, std::io::Error)> for LoadError {
    fn from((path, err): (PathBuf, std::io::Error)) -> Self {
        LoadError { path, kind: LoadErrorKind::Io(err) }
    }
}

impl From<(PathBuf, toml::de::Error)> for LoadError {
    fn from((path, err): (PathBuf, toml::de::Error)) -> Self {
        LoadError { path, kind: LoadErrorKind::Parse(err) }
    }
}

impl std::error::Error for LoadError {}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.kind {
            LoadErrorKind::Io(e) => {
                write!(f, "read \"{}\": {}", self.path.display(), e)
            }
            LoadErrorKind::Parse(e) => {
                write!(f, "parse \"{}\": {}", self.path.display(), e)
            }
            LoadErrorKind::InvalidTunable(inner) => {
                write!(
                    f,
                    "invalid tunable \"{}\": {}",
                    self.path.display(),
                    inner,
                )
            }
        }
    }
}

impl std::cmp::PartialEq<std::io::Error> for LoadError {
    fn eq(&self, other: &std::io::Error) -> bool {
        if let LoadErrorKind::Io(e) = &self.kind {
            e.kind() == other.kind()
        } else {
            false
        }
    }
}

/// List of supported external authn schemes
///
/// Note that the authn subsystem doesn't know about this type.  It allows
/// schemes to be called whatever they want.  This is just to provide a set of
/// allowed values for configuration.
#[derive(
    Clone, Copy, Debug, DeserializeFromStr, Eq, PartialEq, SerializeDisplay,
)]
pub enum SchemeName {
    Spoof,
    SessionCookie,
}

impl std::str::FromStr for SchemeName {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "spoof" => Ok(SchemeName::Spoof),
            "session_cookie" => Ok(SchemeName::SessionCookie),
            _ => Err(anyhow!("unsupported authn scheme: {:?}", s)),
        }
    }
}

impl std::fmt::Display for SchemeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SchemeName::Spoof => "spoof",
            SchemeName::SessionCookie => "session_cookie",
        })
    }
}

impl Config {
    /// Load a `Config` from the given TOML file
    ///
    /// This config object can then be used to create a new `Nexus`.
    /// The format is described in the README.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Config, LoadError> {
        let path = path.as_ref();
        let file_contents = std::fs::read_to_string(path)
            .map_err(|e| (path.to_path_buf(), e))?;
        let config_parsed: Config = toml::from_str(&file_contents)
            .map_err(|e| (path.to_path_buf(), e))?;
        Ok(config_parsed)
    }
}

#[cfg(test)]
mod test {
    use super::Tunables;
    use super::{
        AuthnConfig, Config, ConsoleConfig, LoadError, LoadErrorKind,
        SchemeName, TimeseriesDbConfig, UpdatesConfig,
    };
    use crate::db;
    use dropshot::ConfigDropshot;
    use dropshot::ConfigLogging;
    use dropshot::ConfigLoggingIfExists;
    use dropshot::ConfigLoggingLevel;
    use libc;
    use std::fs;
    use std::net::SocketAddr;
    use std::path::Path;
    use std::path::PathBuf;

    /// Generates a temporary filesystem path unique for the given label.
    fn temp_path(label: &str) -> PathBuf {
        let arg0str = std::env::args().next().expect("expected process arg0");
        let arg0 = Path::new(&arg0str)
            .file_name()
            .expect("expected arg0 filename")
            .to_str()
            .expect("expected arg0 filename to be valid Unicode");
        let pid = std::process::id();
        let mut pathbuf = std::env::temp_dir();
        pathbuf.push(format!("{}.{}.{}", arg0, pid, label));
        pathbuf
    }

    /// Load a Config with the given string `contents`.  To exercise
    /// the full path, this function writes the contents to a file first, then
    /// loads the config from that file, then removes the file.  `label` is used
    /// as a unique string for the filename and error messages.  It should be
    /// unique for each test.
    fn read_config(label: &str, contents: &str) -> Result<Config, LoadError> {
        let pathbuf = temp_path(label);
        let path = pathbuf.as_path();
        eprintln!("writing test config {}", path.display());
        fs::write(path, contents).expect("write to tempfile failed");

        let result = Config::from_file(path);
        fs::remove_file(path).expect("failed to remove temporary file");
        eprintln!("{:?}", result);
        result
    }

    // Totally bogus config files (nonexistent, bad TOML syntax)

    #[test]
    fn test_config_nonexistent() {
        let error = Config::from_file(Path::new("/nonexistent"))
            .expect_err("expected config to fail from /nonexistent");
        let expected = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(error, expected);
    }

    #[test]
    fn test_config_bad_toml() {
        let error =
            read_config("bad_toml", "foo =").expect_err("expected failure");
        if let LoadErrorKind::Parse(error) = &error.kind {
            assert_eq!(error.line_col(), Some((0, 5)));
            assert_eq!(
                error.to_string(),
                "unexpected eof encountered at line 1 column 6"
            );
        } else {
            panic!(
                "Got an unexpected error, expected Parse but got {:?}",
                error
            );
        }
    }

    // Empty config (special case of a missing required field, but worth calling
    // out explicitly)

    #[test]
    fn test_config_empty() {
        let error = read_config("empty", "").expect_err("expected failure");
        if let LoadErrorKind::Parse(error) = &error.kind {
            assert_eq!(error.line_col(), None);
            assert_eq!(error.to_string(), "missing field `dropshot_external`");
        } else {
            panic!(
                "Got an unexpected error, expected Parse but got {:?}",
                error
            );
        }
    }

    // Success case.  We don't need to retest semantics for either ConfigLogging
    // or ConfigDropshot because those are both tested within Dropshot.  If we
    // add new configuration sections of our own, we will want to test those
    // here (both syntax and semantics).
    #[test]
    fn test_valid() {
        let config = read_config(
            "valid",
            r##"
            id = "28b90dc4-c22a-65ba-f49a-f051fe01208f"
            [console]
            static_dir = "tests/static"
            cache_control_max_age_minutes = 10
            session_idle_timeout_minutes = 60
            session_absolute_timeout_minutes = 480
            [authn]
            schemes_external = []
            [dropshot_external]
            bind_address = "10.1.2.3:4567"
            request_body_max_bytes = 1024
            [dropshot_internal]
            bind_address = "10.1.2.3:4568"
            request_body_max_bytes = 1024
            [database]
            url = "postgresql://127.0.0.1?sslmode=disable"
            [log]
            mode = "file"
            level = "debug"
            path = "/nonexistent/path"
            if_exists = "fail"
            [timeseries_db]
            address = "[::1]:8123"
            [updates]
            trusted_root = "/path/to/root.json"
            default_base_url = "http://example.invalid/"
            [tunables]
            max_vpc_ipv4_subnet_prefix = 27
            "##,
        )
        .unwrap();

        assert_eq!(
            config,
            Config {
                id: "28b90dc4-c22a-65ba-f49a-f051fe01208f".parse().unwrap(),
                console: ConsoleConfig {
                    static_dir: "tests/static".parse().unwrap(),
                    cache_control_max_age_minutes: 10,
                    session_idle_timeout_minutes: 60,
                    session_absolute_timeout_minutes: 480
                },
                authn: AuthnConfig { schemes_external: Vec::new() },
                dropshot_external: ConfigDropshot {
                    bind_address: "10.1.2.3:4567"
                        .parse::<SocketAddr>()
                        .unwrap(),
                    ..Default::default()
                },
                dropshot_internal: ConfigDropshot {
                    bind_address: "10.1.2.3:4568"
                        .parse::<SocketAddr>()
                        .unwrap(),
                    ..Default::default()
                },
                log: ConfigLogging::File {
                    level: ConfigLoggingLevel::Debug,
                    if_exists: ConfigLoggingIfExists::Fail,
                    path: "/nonexistent/path".to_string()
                },
                database: db::Config {
                    url: "postgresql://127.0.0.1?sslmode=disable"
                        .parse()
                        .unwrap()
                },
                timeseries_db: TimeseriesDbConfig {
                    address: "[::1]:8123".parse().unwrap()
                },
                updates: Some(UpdatesConfig {
                    trusted_root: PathBuf::from("/path/to/root.json"),
                    default_base_url: "http://example.invalid/".into(),
                }),
                tunables: Tunables { max_vpc_ipv4_subnet_prefix: 27 },
            }
        );

        let config = read_config(
            "valid",
            r##"
            id = "28b90dc4-c22a-65ba-f49a-f051fe01208f"
            [console]
            static_dir = "tests/static"
            cache_control_max_age_minutes = 10
            session_idle_timeout_minutes = 60
            session_absolute_timeout_minutes = 480
            [authn]
            schemes_external = [ "spoof", "session_cookie" ]
            [dropshot_external]
            bind_address = "10.1.2.3:4567"
            request_body_max_bytes = 1024
            [dropshot_internal]
            bind_address = "10.1.2.3:4568"
            request_body_max_bytes = 1024
            [database]
            url = "postgresql://127.0.0.1?sslmode=disable"
            [log]
            mode = "file"
            level = "debug"
            path = "/nonexistent/path"
            if_exists = "fail"
            [timeseries_db]
            address = "[::1]:8123"
            "##,
        )
        .unwrap();

        assert_eq!(
            config.authn.schemes_external,
            vec![SchemeName::Spoof, SchemeName::SessionCookie],
        );
    }

    #[test]
    fn test_bad_authn_schemes() {
        let error = read_config(
            "bad authn.schemes_external",
            r##"
            id = "28b90dc4-c22a-65ba-f49a-f051fe01208f"
            [console]
            static_dir = "tests/static"
            cache_control_max_age_minutes = 10
            session_idle_timeout_minutes = 60
            session_absolute_timeout_minutes = 480
            [authn]
            schemes_external = ["trust-me"]
            [dropshot_external]
            bind_address = "10.1.2.3:4567"
            request_body_max_bytes = 1024
            [dropshot_internal]
            bind_address = "10.1.2.3:4568"
            request_body_max_bytes = 1024
            [database]
            url = "postgresql://127.0.0.1?sslmode=disable"
            [log]
            mode = "file"
            level = "debug"
            path = "/nonexistent/path"
            if_exists = "fail"
            [timeseries_db]
            address = "[::1]:8123"
            "##,
        )
        .expect_err("expected failure");
        if let LoadErrorKind::Parse(error) = &error.kind {
            assert!(error.to_string().starts_with(
                "unsupported authn scheme: \"trust-me\" \
                for key `authn.schemes_external`"
            ));
        } else {
            panic!(
                "Got an unexpected error, expected Parse but got {:?}",
                error
            );
        }
    }

    #[test]
    fn test_invalid_ipv4_prefix_tunable() {
        let error = read_config(
            "invalid_ipv4_prefix_tunable",
            r##"
            id = "28b90dc4-c22a-65ba-f49a-f051fe01208f"
            [console]
            static_dir = "tests/static"
            cache_control_max_age_minutes = 10
            session_idle_timeout_minutes = 60
            session_absolute_timeout_minutes = 480
            [authn]
            schemes_external = []
            [dropshot_external]
            bind_address = "10.1.2.3:4567"
            request_body_max_bytes = 1024
            [dropshot_internal]
            bind_address = "10.1.2.3:4568"
            request_body_max_bytes = 1024
            [database]
            url = "postgresql://127.0.0.1?sslmode=disable"
            [log]
            mode = "file"
            level = "debug"
            path = "/nonexistent/path"
            if_exists = "fail"
            [timeseries_db]
            address = "[::1]:8123"
            [updates]
            trusted_root = "/path/to/root.json"
            default_base_url = "http://example.invalid/"
            [tunables]
            max_vpc_ipv4_subnet_prefix = 100
            "##,
        )
        .expect_err("Expected failure");
        if let LoadErrorKind::Parse(error) = &error.kind {
            assert!(error.to_string().starts_with(
                r#"invalid "max_vpc_ipv4_subnet_prefix": "IPv4 subnet prefix must"#,
            ));
        } else {
            panic!(
                "Got an unexpected error, expected Parse but got {:?}",
                error
            );
        }
    }
}
