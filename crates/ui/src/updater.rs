//! Package environment detection and automatic update management.
//!
//! Supports Flatpak, Snap, AppImage, and native environments.
//! Detects available package updates through packaging portals, system daemons,
//! and release endpoints, and executes self-updates automatically.

use std::path::Path;
use std::process::Command as StdCommand;
use std::time::Duration;

/// Packaging environment in which Gungnir FM is currently executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFormat {
    Flatpak,
    Snap,
    AppImage,
    Native,
}

impl PackageFormat {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Flatpak => "flatpak",
            Self::Snap => "snap",
            Self::AppImage => "appimage",
            Self::Native => "native",
        }
    }

    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Flatpak => "Flatpak",
            Self::Snap => "Snap",
            Self::AppImage => "AppImage",
            Self::Native => "Native",
        }
    }
}

/// Information about an available update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub available: bool,
    pub version: String,
    pub format: PackageFormat,
    pub details: String,
}

/// Detect the packaging format of the running binary.
#[must_use]
pub fn detect_package_format() -> PackageFormat {
    if let Ok(val) = std::env::var("GUNGNIR_PACKAGE_FORMAT") {
        match val.to_lowercase().as_str() {
            "flatpak" => return PackageFormat::Flatpak,
            "snap" => return PackageFormat::Snap,
            "appimage" => return PackageFormat::AppImage,
            "native" => return PackageFormat::Native,
            _ => {}
        }
    }

    if Path::new("/.flatpak-info").exists() || std::env::var_os("FLATPAK_ID").is_some() {
        PackageFormat::Flatpak
    } else if std::env::var_os("SNAP").is_some() || std::env::var_os("SNAP_NAME").is_some() {
        PackageFormat::Snap
    } else if std::env::var_os("APPIMAGE").is_some() || std::env::var_os("APPDIR").is_some() {
        PackageFormat::AppImage
    } else {
        PackageFormat::Native
    }
}

/// Parse a version string (e.g. "0.2.0" or "v1.4.2") into (major, minor, patch, pre).
fn parse_version_tuple(v: &str) -> (u64, u64, u64, String) {
    let clean = v.trim().strip_prefix('v').unwrap_or(v.trim());
    let mut parts = clean.splitn(2, '-');
    let numbers = parts.next().unwrap_or("");
    let pre = parts.next().unwrap_or("").to_string();

    let mut nums = numbers.split('.');
    let major = nums.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let minor = nums.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let patch = nums.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);

    (major, minor, patch, pre)
}

/// Returns true if `candidate` is strictly newer than `current`.
#[must_use]
pub fn is_version_newer(candidate: &str, current: &str) -> bool {
    let (c_maj, c_min, c_pat, c_pre) = parse_version_tuple(candidate);
    let (cur_maj, cur_min, cur_pat, cur_pre) = parse_version_tuple(current);

    if (c_maj, c_min, c_pat) > (cur_maj, cur_min, cur_pat) {
        return true;
    }
    if (c_maj, c_min, c_pat) < (cur_maj, cur_min, cur_pat) {
        return false;
    }

    // Both numeric triples are identical: release > pre-release
    c_pre.is_empty() && !cur_pre.is_empty()
}

/// Query whether a newer package update is available.
pub async fn check_for_update(format: PackageFormat) -> UpdateCheckResult {
    // Explicit disable (e.g. during specific offline runs or test environments)
    if std::env::var_os("GUNGNIR_FORCE_NO_UPDATE").is_some() {
        return UpdateCheckResult {
            available: false,
            version: String::new(),
            format,
            details: "Update checks disabled by environment".to_string(),
        };
    }

    // Test override to simulate update availability
    if let Ok(ver) = std::env::var("GUNGNIR_UPDATE_AVAILABLE_VERSION")
        && !ver.is_empty()
    {
        return UpdateCheckResult {
            available: true,
            version: ver.clone(),
            format,
            details: format!("New {} package {} available (simulated)", format.display_name(), ver),
        };
    }

    if cfg!(test) {
        return UpdateCheckResult {
            available: false,
            version: String::new(),
            format,
            details: "Offline unit test".into(),
        };
    }

    // Check environment-specific update mechanisms
    match format {
        PackageFormat::Flatpak => {
            // 1. Try FreeDesktop Portal Updates interface
            if let Ok(res) = check_flatpak_portal_updates().await
                && res.available
            {
                return res;
            }
            // 2. Fall back to remote release query
            check_remote_release(format).await
        }
        PackageFormat::Snap => {
            // 1. Try snapd query if socket/command is available
            if let Ok(res) = check_snap_updates().await
                && res.available
            {
                return res;
            }
            // 2. Fall back to remote release query
            check_remote_release(format).await
        }
        PackageFormat::AppImage | PackageFormat::Native => check_remote_release(format).await,
    }
}

/// Query org.freedesktop.portal.Updates over session bus via zbus.
async fn check_flatpak_portal_updates() -> Result<UpdateCheckResult, String> {
    let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;

    // Call org.freedesktop.portal.Updates.CheckForUpdate on /org/freedesktop/portal/desktop
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Updates",
    )
    .await
    .map_err(|e| e.to_string())?;

    let options: std::collections::HashMap<String, zbus::Variant<'_>> = std::collections::HashMap::new();
    let reply: Result<zbus::OwnedVariant, zbus::Error> = proxy.call("CheckForUpdate", &(options)).await;

    match reply {
        Ok(_) => Ok(UpdateCheckResult {
            available: false, // Portal emits UpdateAvailable signal on positive match
            version: String::new(),
            format: PackageFormat::Flatpak,
            details: "Portal check dispatched".to_string(),
        }),
        Err(e) => Err(format!("Portal check failed: {e}")),
    }
}

/// Query snapd refresh status.
async fn check_snap_updates() -> Result<UpdateCheckResult, String> {
    // Non-blocking query of snap refresh list
    let out =
        tokio::task::spawn_blocking(|| StdCommand::new("snap").args(["refresh", "--list", "gungnir-fm"]).output())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;

    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if let Some(name) = cols.first()
                && *name == "gungnir-fm"
            {
                let version = cols.get(1).unwrap_or(&"latest").to_string();
                return Ok(UpdateCheckResult {
                    available: true,
                    version,
                    format: PackageFormat::Snap,
                    details: "Snap update available from Snap Store".to_string(),
                });
            }
        }
    }

    Ok(UpdateCheckResult {
        available: false,
        version: String::new(),
        format: PackageFormat::Snap,
        details: "No snap update pending".to_string(),
    })
}

/// Fallback check against remote release endpoints.
async fn check_remote_release(format: PackageFormat) -> UpdateCheckResult {
    let current_version = env!("CARGO_PKG_VERSION");

    // Optional custom release endpoint URL
    let url = std::env::var("GUNGNIR_UPDATE_URL")
        .unwrap_or_else(|_| "https://gitlab.com/api/v4/projects/exalted6165%2Fgungnir-fm/repository/tags".to_string());

    // Check remote releases with a quick timeout
    match query_release_version_with_timeout(&url, Duration::from_secs(3)).await {
        Ok(remote_ver) => {
            if is_version_newer(&remote_ver, current_version) {
                UpdateCheckResult {
                    available: true,
                    version: remote_ver.clone(),
                    format,
                    details: format!("New version {} available (current: {})", remote_ver, current_version),
                }
            } else {
                UpdateCheckResult {
                    available: false,
                    version: remote_ver,
                    format,
                    details: format!("Current version {} is up to date", current_version),
                }
            }
        }
        Err(err) => UpdateCheckResult {
            available: false,
            version: String::new(),
            format,
            details: format!("Update check skipped ({err})"),
        },
    }
}

/// Perform HTTP or mock release tag check with strict timeout.
async fn query_release_version_with_timeout(url: &str, timeout: Duration) -> Result<String, String> {
    if url.starts_with("file://") || Path::new(url).exists() {
        let path = url.strip_prefix("file://").unwrap_or(url);
        let content = tokio::fs::read_to_string(path).await.map_err(|e| e.to_string())?;
        return parse_remote_tags_json(&content);
    }

    // Spawn curl with timeout and -- switch terminator to prevent URL parameter injection
    let u = url.to_string();
    let res = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || {
            StdCommand::new("curl").args(["-fsSL", "--connect-timeout", "2", "--max-time", "3", "--", &u]).output()
        }),
    )
    .await
    .map_err(|_| "Release query timed out".to_string())?
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    if res.status.success() {
        let body = String::from_utf8_lossy(&res.stdout);
        parse_remote_tags_json(&body)
    } else {
        Err(format!("Curl exited with code {:?}", res.status.code()))
    }
}

/// Parse GitLab tags JSON response to find latest tag name.
fn parse_remote_tags_json(json_str: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(json_str).map_err(|e| e.to_string())?;
    if let Some(arr) = parsed.as_array() {
        let mut best: Option<String> = None;
        for item in arr {
            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                let tag = name.trim().strip_prefix('v').unwrap_or(name.trim()).to_string();
                if let Some(ref cur) = best {
                    if is_version_newer(&tag, cur) {
                        best = Some(tag);
                    }
                } else {
                    best = Some(tag);
                }
            }
        }
        best.ok_or_else(|| "No valid release tags found".to_string())
    } else if let Some(tag) = parsed.get("version").and_then(|v| v.as_str()) {
        Ok(tag.trim().strip_prefix('v').unwrap_or(tag.trim()).to_string())
    } else {
        Err("Invalid JSON format for release tags".to_string())
    }
}

/// Perform auto-update for the current package format.
pub async fn perform_update(format: PackageFormat, target_version: &str) -> Result<String, String> {
    // In test/simulation mode
    if cfg!(test) || std::env::var_os("GUNGNIR_TEST_UPDATE_SIMULATE").is_some() {
        tokio::time::sleep(Duration::from_millis(200)).await;
        return Ok(format!(
            "Simulated {} package update to {} completed successfully",
            format.display_name(),
            target_version
        ));
    }

    match format {
        PackageFormat::Flatpak => {
            // First attempt: portal install update
            if let Ok(conn) = zbus::Connection::session().await
                && let Ok(proxy) = zbus::Proxy::new(
                    &conn,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Updates",
                )
                .await
            {
                let options: std::collections::HashMap<String, zbus::Variant<'_>> = std::collections::HashMap::new();
                let call: Result<zbus::OwnedVariant, zbus::Error> = proxy.call("InstallUpdate", &("", options)).await;
                if call.is_ok() {
                    return Ok("Flatpak update initiated via desktop portal".to_string());
                }
            }

            // Fallback: flatpak CLI update
            let res = tokio::task::spawn_blocking(|| {
                StdCommand::new("flatpak").args(["update", "-y", "org.gungnir.GungnirFM"]).output()
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;

            if res.status.success() {
                Ok("Flatpak package updated successfully".to_string())
            } else {
                let err = String::from_utf8_lossy(&res.stderr);
                Err(format!("Flatpak update command failed: {err}"))
            }
        }
        PackageFormat::Snap => {
            let res = tokio::task::spawn_blocking(|| StdCommand::new("snap").args(["refresh", "gungnir-fm"]).output())
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;

            if res.status.success() {
                Ok("Snap package refreshed successfully".to_string())
            } else {
                let err = String::from_utf8_lossy(&res.stderr);
                Err(format!("Snap refresh failed: {err}"))
            }
        }
        PackageFormat::AppImage => {
            // Check for AppImageUpdate or appimageupdatetool
            if let Some(appimage_path) = std::env::var_os("APPIMAGE") {
                let path_buf = std::path::PathBuf::from(appimage_path);
                if path_buf.exists() {
                    let tool = if Path::new("/usr/bin/appimageupdatetool").exists() {
                        Some("appimageupdatetool")
                    } else if Path::new("/usr/bin/AppImageUpdate").exists() {
                        Some("AppImageUpdate")
                    } else {
                        None
                    };

                    if let Some(updater) = tool {
                        let pb = path_buf.clone();
                        let res =
                            tokio::task::spawn_blocking(move || StdCommand::new(updater).arg("--").arg(&pb).output())
                                .await
                                .map_err(|e| e.to_string())?
                                .map_err(|e| e.to_string())?;

                        if res.status.success() {
                            return Ok("AppImage updated via AppImageUpdate tool".to_string());
                        }
                    }
                }
            }

            // Repository downloads have no configured delta-update feed.
            Err(format!(
                "Install AppImage {target_version} from https://gitlab.com/exalted6165/gungnir-fm/-/tree/main/packages"
            ))
        }
        PackageFormat::Native => {
            Err(format!("Install Gungnir FM {target_version} from source or your package manager"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison_logic() {
        assert!(is_version_newer("0.2.0", "0.1.0"));
        assert!(is_version_newer("0.1.1", "0.1.0"));
        assert!(is_version_newer("1.0.0", "0.9.9"));
        assert!(is_version_newer("v1.2.3", "1.2.2"));
        assert!(!is_version_newer("0.1.0", "0.1.0"));
        assert!(!is_version_newer("0.0.9", "0.1.0"));
        assert!(!is_version_newer("0.1.0-alpha", "0.1.0"));
        assert!(is_version_newer("0.1.0", "0.1.0-beta"));
    }

    #[test]
    fn test_package_format_detection_and_override() {
        // Test override mechanism
        // SAFETY: Only sets environment variable for unit testing within local test scope
        unsafe {
            std::env::set_var("GUNGNIR_PACKAGE_FORMAT", "flatpak");
        }
        assert_eq!(detect_package_format(), PackageFormat::Flatpak);

        unsafe {
            std::env::set_var("GUNGNIR_PACKAGE_FORMAT", "snap");
        }
        assert_eq!(detect_package_format(), PackageFormat::Snap);

        unsafe {
            std::env::set_var("GUNGNIR_PACKAGE_FORMAT", "appimage");
        }
        assert_eq!(detect_package_format(), PackageFormat::AppImage);

        unsafe {
            std::env::remove_var("GUNGNIR_PACKAGE_FORMAT");
        }
    }

    #[test]
    fn test_parse_remote_tags_json() {
        let json = r#"[
            {"name": "v0.1.0"},
            {"name": "v0.2.0"},
            {"name": "v0.1.5"}
        ]"#;
        let res = parse_remote_tags_json(json);
        assert_eq!(res.ok(), Some("0.2.0".to_string()));

        let single = r#"{"version": "0.3.0"}"#;
        assert_eq!(parse_remote_tags_json(single).ok(), Some("0.3.0".to_string()));
    }

    #[tokio::test]
    async fn test_simulated_update_check_and_perform() {
        unsafe {
            std::env::set_var("GUNGNIR_UPDATE_AVAILABLE_VERSION", "0.5.0");
            std::env::set_var("GUNGNIR_TEST_UPDATE_SIMULATE", "1");
        }

        let res = check_for_update(PackageFormat::Flatpak).await;
        assert!(res.available);
        assert_eq!(res.version, "0.5.0");

        let update_res = perform_update(PackageFormat::Flatpak, "0.5.0").await;
        assert!(update_res.is_ok());

        unsafe {
            std::env::remove_var("GUNGNIR_UPDATE_AVAILABLE_VERSION");
            std::env::remove_var("GUNGNIR_TEST_UPDATE_SIMULATE");
        }
    }
}
