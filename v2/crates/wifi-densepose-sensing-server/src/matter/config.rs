//! Matter bridge settings from the command line (ADR-431 spike).
//!
//! No Matter SDK code here, so the checks are tested without the `matter`
//! feature.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::MatterArgs;

/// The CSA test vendor ID.
pub const TEST_VENDOR_ID: u16 = 0xFFF1;
/// The product ID of the test device attestation the bridge ships with.
pub const TEST_PRODUCT_ID: u16 = 0x8001;

/// What the bridge needs to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Where the bridge keeps its paired controllers (fabrics) and keys.
    pub storage_dir: PathBuf,
    /// Forget every paired controller before starting.
    pub reset: bool,
    /// Where to write the setup code while the bridge isn't paired.
    pub setup_file: Option<PathBuf>,
    /// SYNTHETIC test pattern: flip occupancy at this period instead of
    /// following sensing (`--matter-test-toggle-secs`).
    pub test_toggle: Option<Duration>,
}

impl BridgeConfig {
    /// Check the flags and build the settings. `data_dir` is the server's
    /// data directory; the bridge keeps its state in `matter/` under it.
    ///
    /// The spike only has the CSA test attestation for 0xFFF1/0x8001.
    /// Another vendor or product ID would fail device attestation during
    /// pairing, so it is refused here with the reason.
    pub fn from_args(args: &MatterArgs, data_dir: &Path) -> Result<Self, String> {
        let vendor_id = parse_id(&args.matter_vendor_id).ok_or_else(|| {
            format!(
                "--matter-vendor-id '{}' is not a 16-bit ID",
                args.matter_vendor_id
            )
        })?;
        let product_id = parse_id(&args.matter_product_id).ok_or_else(|| {
            format!(
                "--matter-product-id '{}' is not a 16-bit ID",
                args.matter_product_id
            )
        })?;
        if (vendor_id, product_id) != (TEST_VENDOR_ID, TEST_PRODUCT_ID) {
            return Err(format!(
                "this build only has the CSA test attestation for vendor 0x{TEST_VENDOR_ID:04X} \
                 product 0x{TEST_PRODUCT_ID:04X} (ADR-431); 0x{vendor_id:04X}/0x{product_id:04X} \
                 would fail device attestation during pairing"
            ));
        }
        if let Some(path) = &args.matter_setup_file {
            if path.as_os_str().is_empty() || path.is_dir() {
                return Err(format!(
                    "--matter-setup-file '{}' must be a file path",
                    path.display()
                ));
            }
        }
        Ok(Self {
            storage_dir: data_dir.join("matter"),
            reset: args.matter_reset,
            setup_file: args.matter_setup_file.clone(),
            test_toggle: args.matter_test_toggle_secs.map(Duration::from_secs),
        })
    }
}

/// Parse a vendor or product ID: `0xFFF1`, `0XFFF1` or decimal `65521`.
pub fn parse_id(s: &str) -> Option<u16> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(vid: &str, pid: &str) -> MatterArgs {
        MatterArgs {
            matter: true,
            matter_setup_file: None,
            matter_reset: false,
            matter_vendor_id: vid.into(),
            matter_product_id: pid.into(),
            matter_test_toggle_secs: None,
        }
    }

    #[test]
    fn parses_hex_and_decimal_ids() {
        assert_eq!(parse_id("0xFFF1"), Some(0xFFF1));
        assert_eq!(parse_id("0Xfff1"), Some(0xFFF1));
        assert_eq!(parse_id(" 32769 "), Some(0x8001));
        assert_eq!(parse_id("0x10000"), None);
        assert_eq!(parse_id("fff1"), None);
        assert_eq!(parse_id(""), None);
    }

    #[test]
    fn defaults_are_the_test_identity() {
        let cfg = BridgeConfig::from_args(&args("0xFFF1", "0x8001"), Path::new("/data")).unwrap();
        assert_eq!(cfg.storage_dir, Path::new("/data/matter"));
        assert!(!cfg.reset);
        assert_eq!(cfg.setup_file, None);
        assert_eq!(cfg.test_toggle, None);
    }

    #[test]
    fn test_toggle_is_carried_as_a_duration() {
        let mut a = args("0xFFF1", "0x8001");
        a.matter_test_toggle_secs = Some(30);
        let cfg = BridgeConfig::from_args(&a, Path::new("/d")).unwrap();
        assert_eq!(cfg.test_toggle, Some(Duration::from_secs(30)));
    }

    #[test]
    fn other_ids_are_refused_with_the_reason() {
        let err = BridgeConfig::from_args(&args("0x1234", "0x8001"), Path::new("/d")).unwrap_err();
        assert!(err.contains("attestation"), "{err}");
        let err = BridgeConfig::from_args(&args("0xFFF1", "0x8000"), Path::new("/d")).unwrap_err();
        assert!(err.contains("0xFFF1/0x8000"), "{err}");
        assert!(BridgeConfig::from_args(&args("nope", "0x8001"), Path::new("/d")).is_err());
    }

    #[test]
    fn setup_file_must_not_be_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = args("0xFFF1", "0x8001");
        a.matter_setup_file = Some(dir.path().to_path_buf());
        assert!(BridgeConfig::from_args(&a, dir.path()).is_err());
        a.matter_setup_file = Some(dir.path().join("matter-setup.txt"));
        let cfg = BridgeConfig::from_args(&a, dir.path()).unwrap();
        assert_eq!(cfg.setup_file, Some(dir.path().join("matter-setup.txt")));
    }
}
