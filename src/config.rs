//! Service TOML configuration: socket path and exact opaque pin mappings.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;

/// Environment variable that can supply the config file path.
pub const CONFIG_ENV_VAR: &str = "GPIOJSONSVC_CONFIG";

/// Default config file name when neither a positional path nor `GPIOJSONSVC_CONFIG` is set.
pub const DEFAULT_CONFIG_PATH: &str = "gpiojsonsvc.toml";

/// libgpiod location for one protocol pin string: device path plus line offset.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GPIODPinSpec {
    pub device: String,
    pub line: u32,
}

#[derive(Debug, Deserialize)]
struct RawService {
    socket: String,
}

#[derive(Debug, Deserialize)]
struct RawPins {
    gpiod: BTreeMap<String, GPIODPinSpec>,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    service: RawService,
    pins: RawPins,
}

/// Validated service settings and full-string pin maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    pub socket: String,
    gpiod_pins: BTreeMap<String, GPIODPinSpec>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config{}: {source}", path.as_ref().map(|path| format!(" {}", path.display())).unwrap_or_default())]
    Parse {
        path: Option<PathBuf>,
        #[source]
        source: toml::de::Error,
    },
    #[error("service.socket must be a non-empty path")]
    EmptySocket,
    #[error("config [pins.gpiod] must contain at least one mapping")]
    EmptyPinMap,
    #[error("pin mapping keys must be non-empty")]
    EmptyPinKey,
    #[error("pin `{pin}` has an empty device path")]
    EmptyDevice { pin: String },
}

/// Resolve the config file path: positional CLI path, then `GPIOJSONSVC_CONFIG`, then `gpiojsonsvc.toml`.
pub fn resolve_config_path(cli_path: Option<&Path>, env_path: Option<&str>) -> PathBuf {
    if let Some(path) = cli_path {
        return path.to_owned();
    }
    if let Some(path) = env_path.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    PathBuf::from(DEFAULT_CONFIG_PATH)
}

impl ServiceConfig {
    /// Load and validate TOML from a string.
    pub fn from_toml_str(contents: &str) -> Result<Self, ConfigError> {
        Self::from_toml_str_at(contents, None)
    }

    /// Load and validate TOML from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml_str_at(&contents, Some(path.to_owned()))
    }

    /// Discover the config file, then load it.
    pub fn load(cli_path: Option<&Path>) -> Result<Self, ConfigError> {
        let env_path = std::env::var(CONFIG_ENV_VAR).ok();
        let path = resolve_config_path(cli_path, env_path.as_deref());
        Self::load_from_path(&path)
    }

    /// Exact lookup of a complete protocol pin string in `[pins.gpiod]`.
    /// No rewriting or case folding.
    pub fn resolve_gpiod_pin(&self, pin: &str) -> Option<&GPIODPinSpec> {
        self.gpiod_pins.get(pin)
    }

    pub fn gpiod_pins(&self) -> &BTreeMap<String, GPIODPinSpec> {
        &self.gpiod_pins
    }

    fn from_toml_str_at(contents: &str, path: Option<PathBuf>) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            toml::from_str(contents).map_err(|source| ConfigError::Parse { path, source })?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        if raw.service.socket.is_empty() {
            return Err(ConfigError::EmptySocket);
        }
        if raw.pins.gpiod.is_empty() {
            return Err(ConfigError::EmptyPinMap);
        }

        for (pin, spec) in &raw.pins.gpiod {
            if pin.is_empty() {
                return Err(ConfigError::EmptyPinKey);
            }
            if spec.device.is_empty() {
                return Err(ConfigError::EmptyDevice { pin: pin.clone() });
            }
        }

        Ok(Self {
            socket: raw.service.socket,
            gpiod_pins: raw.pins.gpiod,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::ConfigError;
    use super::DEFAULT_CONFIG_PATH;
    use super::GPIODPinSpec;
    use super::ServiceConfig;
    use super::resolve_config_path;

    const SAMPLE: &str = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"gpiochip0:7" = { device = "/path/to/gpiochip0.xml", line = 7 }
"GPIO1_B5" = { device = "/path/to/gpiochip1.xml", line = 13 }
"#;

    fn assert_sample(config: &ServiceConfig) {
        assert_eq!(config.socket, "/tmp/gpiojsonsvc.sock");
        assert_eq!(
            config.resolve_gpiod_pin("gpiochip0:7"),
            Some(&GPIODPinSpec {
                device: "/path/to/gpiochip0.xml".to_owned(),
                line: 7,
            })
        );
        assert_eq!(
            config.resolve_gpiod_pin("GPIO1_B5"),
            Some(&GPIODPinSpec {
                device: "/path/to/gpiochip1.xml".to_owned(),
                line: 13,
            })
        );
    }

    #[test]
    fn maps_opaque_pin_strings_by_exact_key() {
        let config = ServiceConfig::from_toml_str(SAMPLE).expect("sample config");
        assert_sample(&config);
        assert_eq!(config.gpiod_pins().len(), 2);
    }

    #[test]
    fn lookup_is_exact_and_does_not_infer_device_or_line() {
        let config = ServiceConfig::from_toml_str(SAMPLE).expect("sample config");

        assert!(config.resolve_gpiod_pin("GPIOCHIP0:7").is_none());
        assert!(config.resolve_gpiod_pin("gpiochip0: 7").is_none());
        assert!(config.resolve_gpiod_pin("gpiochip0:07").is_none());
        assert!(config.resolve_gpiod_pin("gpio1_b5").is_none());
        assert!(config.resolve_gpiod_pin("GPIO1_B5 ").is_none());
        assert!(
            config
                .resolve_gpiod_pin("/path/to/gpiochip0.xml:7")
                .is_none()
        );
    }

    #[test]
    fn two_pin_strings_may_share_one_device() {
        let toml = r#"
[service]
socket = "/run/gpiojsonsvc.sock"

[pins.gpiod]
"gpiochip0:7" = { device = "/dev/gpiochip0", line = 7 }
"GPIO1_B5" = { device = "/dev/gpiochip0", line = 13 }
"#;
        let config = ServiceConfig::from_toml_str(toml).expect("shared device");
        let first = config.resolve_gpiod_pin("gpiochip0:7").expect("first pin");
        let second = config.resolve_gpiod_pin("GPIO1_B5").expect("second pin");
        assert_eq!(first.device, second.device);
        assert_eq!(first.device, "/dev/gpiochip0");
        assert_eq!(first.line, 7);
        assert_eq!(second.line, 13);
    }

    #[test]
    fn rejects_empty_pin_map() {
        let toml = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("empty pins");
        assert!(matches!(error, ConfigError::EmptyPinMap));
    }

    #[test]
    fn rejects_empty_pin_key() {
        let toml = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"" = { device = "/dev/gpiochip0", line = 0 }
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("empty key");
        assert!(matches!(error, ConfigError::EmptyPinKey));
    }

    #[test]
    fn rejects_empty_device() {
        let toml = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"GPIO1_B5" = { device = "", line = 13 }
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("empty device");
        match error {
            ConfigError::EmptyDevice { pin } => assert_eq!(pin, "GPIO1_B5"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn rejects_empty_socket() {
        let toml = r#"
[service]
socket = ""

[pins.gpiod]
"GPIO1_B5" = { device = "/dev/gpiochip0", line = 13 }
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("empty socket");
        assert!(matches!(error, ConfigError::EmptySocket));
    }

    #[test]
    fn rejects_missing_device_field() {
        let toml = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"GPIO1_B5" = { line = 13 }
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("missing device");
        assert!(matches!(error, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_missing_line_field() {
        let toml = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"GPIO1_B5" = { device = "/dev/gpiochip0" }
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("missing line");
        assert!(matches!(error, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_non_u32_line() {
        let toml = r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"

[pins.gpiod]
"GPIO1_B5" = { device = "/dev/gpiochip0", line = -1 }
"#;
        let error = ServiceConfig::from_toml_str(toml).expect_err("negative line");
        assert!(matches!(error, ConfigError::Parse { .. }));
    }

    #[test]
    fn resolve_config_path_prefers_cli_then_env_then_default() {
        let cli = Path::new("/explicit/cli.toml");
        assert_eq!(
            resolve_config_path(Some(cli), Some("/from/env.toml")),
            PathBuf::from("/explicit/cli.toml")
        );
        assert_eq!(
            resolve_config_path(None, Some("/from/env.toml")),
            PathBuf::from("/from/env.toml")
        );
        assert_eq!(
            resolve_config_path(None, Some("")),
            PathBuf::from(DEFAULT_CONFIG_PATH)
        );
        assert_eq!(
            resolve_config_path(None, None),
            PathBuf::from(DEFAULT_CONFIG_PATH)
        );
    }

    #[test]
    fn load_from_path_reads_validated_toml() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("gpiojsonsvc.toml");
        fs::write(&path, SAMPLE).expect("write config");
        let config = ServiceConfig::load_from_path(&path).expect("load");
        assert_sample(&config);
    }

    #[test]
    fn load_from_path_reports_missing_file() {
        let path = Path::new("/no/such/gpiojsonsvc.toml");
        let error = ServiceConfig::load_from_path(path).expect_err("missing file");
        match error {
            ConfigError::Read { path: reported, .. } => assert_eq!(reported, path),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn load_discovers_path_then_reads_file() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("from-cli.toml");
        fs::write(&path, SAMPLE).expect("write config");
        let config = ServiceConfig::load(Some(&path)).expect("load via cli path");
        assert_sample(&config);
    }
}
