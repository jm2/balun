//! Self-contained Windows and macOS package layouts and the hidden
//! packaged-runtime probe.
//!
//! The Windows package keeps the MSYS2 prefix shape Balun was built and
//! probed against: `bin\balun.exe` beside every DLL, `lib\gstreamer-1.0` for
//! the plugin closure, and `libexec\gstreamer-1.0\gst-plugin-scanner.exe`.
//! GStreamer derives the plugin directory and the scanner location from its
//! own DLL and prepends that DLL directory to the scanner's `PATH`, so an
//! ordinary launch needs no environment variable. Balun sets none: writing the
//! process environment is not possible in safe Rust, and the package must not
//! depend on it.
//!
//! Each platform packaging path is the only caller of the probe and owns its
//! environment. Windows supplies a fresh `GST_REGISTRY` and clears the other
//! runtime variables. The macOS launcher additionally supplies only the
//! package-matching plugin directory and scanner plus an empty system plugin
//! path. Both paths clear GIO and proxy overrides. The probe rejects values
//! that could redirect it outside the package, proves the bundled scanner
//! starts, runs the packaged playback probe, requires the fresh registry to
//! exist outside the package, and only then writes its sentinel.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Hidden argument the Windows and macOS packaging helpers pass with a fresh, empty
/// cache root. Nothing else is accepted on the command line.
pub const PLATFORM_RUNTIME_PROBE_FLAG: &str = "--balun-platform-runtime-probe";
/// File the probe writes into its cache root after every check passed.
pub const WINDOWS_PROBE_SENTINEL_NAME: &str = "balun-platform-runtime-probe.ok";
/// Exact sentinel content; the packaging helper compares the bytes.
pub const WINDOWS_PROBE_SENTINEL: &[u8] = b"balun-windows-runtime-probe-v1\n";
/// File the macOS probe writes into its cache root after every check passed.
pub const MACOS_PROBE_SENTINEL_NAME: &str = "balun-platform-runtime-probe.ok";
/// Exact sentinel content for macOS; the packaging helper compares the bytes.
pub const MACOS_PROBE_SENTINEL: &[u8] = b"balun-macos-runtime-probe-v1\n";
/// GStreamer variable naming the fresh registry on every packaged probe.
pub const REGISTRY_ENVIRONMENT_KEY: &str = "GST_REGISTRY";

/// Result of preparing the platform runtime before the toolkit starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolkitPreparation {
    /// Start the desktop application normally.
    Continue,
    /// The hidden packaging probe ran and passed; exit without the toolkit.
    ProbeCompleted,
}

/// Fixed, path-free reason the platform runtime could not be prepared.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PlatformRuntimeError {
    /// The probe flag appeared more than once.
    #[error("the platform runtime probe flag may only be supplied once")]
    DuplicateProbeFlag,
    /// The probe flag had no cache root after it.
    #[error("the platform runtime probe requires an explicit cache root")]
    MissingCacheRoot,
    /// The probe was requested on a platform without a self-contained package.
    #[error("the platform runtime probe is only available in the self-contained package")]
    ProbeUnsupported,
    /// The executable is not inside the documented package layout.
    #[error("the platform runtime probe requires the self-contained Windows package layout")]
    PackageLayout,
    /// The executable is not inside the documented macOS bundle layout.
    #[error("the platform runtime probe requires the self-contained macOS bundle layout")]
    MacBundleLayout,
    /// The cache root was relative.
    #[error("the platform runtime probe cache root must be absolute")]
    CacheRootNotAbsolute,
    /// The cache root contained `.` or `..` components.
    #[error("the platform runtime probe cache root must not contain relative components")]
    CacheRootRelativeComponents,
    /// The cache root resolved inside the package.
    #[error("the platform runtime probe cache root must be outside the application install")]
    CacheRootInsideInstall,
    /// The cache root existed and was not an empty directory.
    #[error("the platform runtime probe cache root must be a fresh, empty directory")]
    CacheRootNotFresh,
    /// The cache root could not be created or resolved.
    #[error("the platform runtime probe cache root could not be prepared")]
    CacheRootUnavailable,
    /// A GStreamer, GIO, or proxy variable other than the registry was set.
    #[error("the platform runtime probe requires inherited {0} to be unset")]
    InheritedEnvironment(String),
    /// `GST_REGISTRY` was absent, relative, or outside the cache root.
    #[error(
        "the platform runtime probe requires GST_REGISTRY to name a file inside its cache root"
    )]
    RegistryEnvironment,
    /// The bundled scanner file is absent.
    #[error("the bundled GStreamer plugin scanner is missing")]
    ScannerMissing,
    /// The bundled scanner could not be spawned.
    #[error("the bundled GStreamer plugin scanner could not start")]
    ScannerStart,
    /// The bundled scanner exited with an unexpected status.
    #[error("the bundled GStreamer plugin scanner returned an unexpected status")]
    ScannerStatus,
    /// The bundled scanner did not exit inside its bound.
    #[error("the bundled GStreamer plugin scanner exceeded its 5-second deadline")]
    ScannerDeadline,
    /// The bundled scanner could not be stopped after the bound.
    #[error("the bundled GStreamer plugin scanner could not be terminated")]
    ScannerTermination,
    /// GStreamer did not create a non-empty registry at the requested path.
    #[error("the platform runtime probe registry was not created")]
    RegistryMissing,
    /// The registry resolved inside the package.
    #[error("the platform runtime probe registry resolves inside the application package")]
    RegistryInsideInstall,
    /// The sentinel could not be written atomically.
    #[error("the platform runtime probe could not write its sentinel")]
    Sentinel,
    /// The packaged playback probe failed.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[error("the packaged playback probe failed: {0}")]
    Probe(#[from] super::packaged_probe::PackagedProbeError),
}

/// Inspect the command line before GTK or GStreamer initialize.
///
/// Returns [`ToolkitPreparation::ProbeCompleted`] only when the hidden
/// packaging probe ran successfully; the process should then exit without
/// starting the application.
pub fn configure_before_toolkit() -> Result<ToolkitPreparation, PlatformRuntimeError> {
    let probe_root = parse_platform_runtime_probe_request(env::args_os())?;
    configure_for_platform(probe_root.as_deref())
}

#[cfg(target_os = "windows")]
/// Run the probe when one was requested; an ordinary launch continues untouched.
fn configure_for_platform(
    probe_root: Option<&Path>,
) -> Result<ToolkitPreparation, PlatformRuntimeError> {
    let Some(probe_root) = probe_root else {
        return Ok(ToolkitPreparation::Continue);
    };
    let exe = env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|_| PlatformRuntimeError::PackageLayout)?;
    let layout = detect_windows_package(&exe).ok_or(PlatformRuntimeError::PackageLayout)?;
    run_windows_runtime_probe(&layout, probe_root)?;
    Ok(ToolkitPreparation::ProbeCompleted)
}

#[cfg(target_os = "macos")]
/// Run the probe when one was requested on macOS; an ordinary launch continues untouched.
fn configure_for_platform(
    probe_root: Option<&Path>,
) -> Result<ToolkitPreparation, PlatformRuntimeError> {
    let Some(probe_root) = probe_root else {
        return Ok(ToolkitPreparation::Continue);
    };
    let exe = env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|_| PlatformRuntimeError::MacBundleLayout)?;
    let layout = detect_macos_bundle(&exe).ok_or(PlatformRuntimeError::MacBundleLayout)?;
    run_macos_runtime_probe(&layout, probe_root)?;
    Ok(ToolkitPreparation::ProbeCompleted)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
/// Refuse the probe outside the Windows or macOS packages and continue otherwise.
fn configure_for_platform(
    probe_root: Option<&Path>,
) -> Result<ToolkitPreparation, PlatformRuntimeError> {
    if probe_root.is_some() {
        return Err(PlatformRuntimeError::ProbeUnsupported);
    }
    Ok(ToolkitPreparation::Continue)
}

/// The self-contained Windows package as seen from the running executable.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsPackageLayout {
    install_root: PathBuf,
    bin_dir: PathBuf,
    plugin_dir: PathBuf,
    scanner: PathBuf,
}

/// Recognize the documented package shape around a canonical executable path.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn detect_windows_package(exe: &Path) -> Option<WindowsPackageLayout> {
    let bin_dir = exe.parent()?.to_path_buf();
    if !bin_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        return None;
    }
    let install_root = bin_dir.parent()?.to_path_buf();
    let plugin_dir = install_root.join("lib").join("gstreamer-1.0");
    if !exe.is_file() || !plugin_dir.is_dir() {
        return None;
    }
    Some(WindowsPackageLayout {
        scanner: install_root
            .join("libexec")
            .join("gstreamer-1.0")
            .join("gst-plugin-scanner.exe"),
        install_root,
        bin_dir,
        plugin_dir,
    })
}

#[cfg(target_os = "windows")]
/// Run every probe stage in order and write the sentinel last.
fn run_windows_runtime_probe(
    layout: &WindowsPackageLayout,
    requested_cache_root: &Path,
) -> Result<(), PlatformRuntimeError> {
    reject_inherited_probe_environment(env::vars_os().map(|(key, _)| key))?;
    if !layout.scanner.is_file() {
        return Err(PlatformRuntimeError::ScannerMissing);
    }
    let cache_root = validate_probe_cache_root(requested_cache_root, &layout.install_root)?;
    let registry = probe_registry_path(
        env::var_os(REGISTRY_ENVIRONMENT_KEY).as_deref(),
        &cache_root,
    )?;
    preflight_windows_plugin_scanner(&layout.scanner, &layout.bin_dir)?;
    super::packaged_probe::run(&layout.plugin_dir)?;
    verify_probe_registry(&registry, &layout.install_root)?;
    atomic_replace(
        &cache_root.join(WINDOWS_PROBE_SENTINEL_NAME),
        WINDOWS_PROBE_SENTINEL,
    )?;
    Ok(())
}

/// The self-contained macOS application bundle as seen from the running executable.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacBundleLayout {
    pub app_root: PathBuf,
    pub contents_dir: PathBuf,
    pub macos_dir: PathBuf,
    pub resources_dir: PathBuf,
    pub plugin_dir: PathBuf,
    pub scanner: PathBuf,
}

/// Recognize the documented .app bundle shape around a canonical executable path.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn detect_macos_bundle(exe: &Path) -> Option<MacBundleLayout> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()? != "Contents" {
        return None;
    }
    let app_root = contents_dir.parent()?;
    if app_root.extension()? != "app" {
        return None;
    }
    let plist_path = contents_dir.join("Info.plist");
    let resources_dir = contents_dir.join("Resources");
    let plugin_dir = resources_dir.join("lib").join("gstreamer-1.0");
    let scanner = macos_dir.join("gst-plugin-scanner");

    if !exe.is_file() || !plist_path.is_file() || !resources_dir.is_dir() || !plugin_dir.is_dir() {
        return None;
    }

    let plist = std::fs::read(&plist_path).ok()?;
    if plist.len() > 1024 * 1024 {
        return None;
    }
    let plist_text = std::str::from_utf8(&plist).ok()?;
    if !plist_text.contains("<key>CFBundlePackageType</key>")
        || !plist_text.contains("<string>APPL</string>")
        || !plist_text.contains("<key>CFBundleExecutable</key>")
    {
        return None;
    }

    Some(MacBundleLayout {
        app_root: app_root.to_path_buf(),
        contents_dir: contents_dir.to_path_buf(),
        macos_dir: macos_dir.to_path_buf(),
        resources_dir,
        plugin_dir,
        scanner,
    })
}

/// Check if any mutable cache files exist inside the signed application bundle.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn bundle_contains_mutable_cache(layout: &MacBundleLayout) -> bool {
    layout.macos_dir.join("gst-registry.bin").exists()
        || layout
            .resources_dir
            .join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache")
            .exists()
        || layout
            .resources_dir
            .join("lib/gstreamer-1.0/gst-registry.bin")
            .exists()
}

/// User runtime cache paths segregated by platform, architecture, and install path hash.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCachePaths {
    pub root: PathBuf,
    pub gst_registry: PathBuf,
    pub pixbuf_loaders: PathBuf,
}

/// 64-bit FNV-1a hash of the canonical install root path.
#[allow(dead_code)]
pub fn stable_path_fingerprint(path: &Path) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Calculate dedicated, install-keyed user cache paths.
#[allow(dead_code)]
pub fn runtime_cache_paths(
    cache_base: &Path,
    platform: &str,
    architecture: &str,
    install_root: &Path,
) -> Result<RuntimeCachePaths, PlatformRuntimeError> {
    if !cache_base.is_absolute() {
        return Err(PlatformRuntimeError::CacheRootNotAbsolute);
    }
    if has_relative_components(cache_base) {
        return Err(PlatformRuntimeError::CacheRootRelativeComponents);
    }
    if platform.is_empty() || architecture.is_empty() {
        return Err(PlatformRuntimeError::CacheRootUnavailable);
    }

    let install_key = stable_path_fingerprint(install_root);
    let root = cache_base
        .join("balun")
        .join("runtime")
        .join(format!("{platform}-{architecture}"))
        .join(format!("{install_key:016x}"));

    Ok(RuntimeCachePaths {
        gst_registry: root.join("gstreamer").join("registry.bin"),
        pixbuf_loaders: root.join("gdk-pixbuf").join("loaders.cache"),
        root,
    })
}

#[cfg(target_os = "macos")]
/// Run every probe stage in order and write the sentinel last on macOS.
fn run_macos_runtime_probe(
    layout: &MacBundleLayout,
    requested_cache_root: &Path,
) -> Result<(), PlatformRuntimeError> {
    reject_inherited_macos_probe_environment(env::vars_os().map(|(key, _)| key), layout)?;
    if bundle_contains_mutable_cache(layout) {
        return Err(PlatformRuntimeError::RegistryInsideInstall);
    }
    if !layout.scanner.is_file() {
        return Err(PlatformRuntimeError::ScannerMissing);
    }
    let cache_root = validate_probe_cache_root(requested_cache_root, &layout.app_root)?;
    let registry = probe_registry_path(
        env::var_os(REGISTRY_ENVIRONMENT_KEY).as_deref(),
        &cache_root,
    )?;
    preflight_macos_plugin_scanner(&layout.scanner)?;
    super::packaged_probe::run(&layout.plugin_dir)?;
    verify_probe_registry(&registry, &layout.app_root)?;
    if bundle_contains_mutable_cache(layout) {
        return Err(PlatformRuntimeError::RegistryInsideInstall);
    }
    atomic_replace(
        &cache_root.join(MACOS_PROBE_SENTINEL_NAME),
        MACOS_PROBE_SENTINEL,
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn reject_inherited_macos_probe_environment(
    keys: impl IntoIterator<Item = OsString>,
    layout: &MacBundleLayout,
) -> Result<(), PlatformRuntimeError> {
    let mut forbidden = Vec::new();
    for key in keys {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if matches!(
            name.as_str(),
            "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "NO_PROXY"
                | "GIO_EXTRA_MODULES"
                | "GIO_USE_PROXY_RESOLVER"
        ) {
            forbidden.push(name);
            continue;
        }
        if name.starts_with("GST_") {
            match name.as_str() {
                "GST_REGISTRY" | "GST_REGISTRY_1_0" => {}
                "GST_PLUGIN_SYSTEM_PATH" | "GST_PLUGIN_SYSTEM_PATH_1_0" => {
                    if env::var_os(&key).is_some_and(|val| !val.is_empty()) {
                        forbidden.push(name);
                    }
                }
                "GST_PLUGIN_PATH" | "GST_PLUGIN_PATH_1_0" => {
                    if let Some(val) = env::var_os(&key) {
                        let p = PathBuf::from(val);
                        if let Ok(canon) = p.canonicalize() {
                            if let Ok(expected) = layout.plugin_dir.canonicalize() {
                                if canon != expected {
                                    forbidden.push(name);
                                }
                            } else {
                                forbidden.push(name);
                            }
                        } else {
                            forbidden.push(name);
                        }
                    }
                }
                "GST_PLUGIN_SCANNER" | "GST_PLUGIN_SCANNER_1_0" => {
                    if let Some(val) = env::var_os(&key) {
                        let p = PathBuf::from(val);
                        if let Ok(canon) = p.canonicalize() {
                            if let Ok(expected) = layout.scanner.canonicalize() {
                                if canon != expected {
                                    forbidden.push(name);
                                }
                            } else {
                                forbidden.push(name);
                            }
                        } else {
                            forbidden.push(name);
                        }
                    }
                }
                _ => forbidden.push(name),
            }
        }
    }
    forbidden.sort();
    match forbidden.into_iter().next() {
        Some(name) => Err(PlatformRuntimeError::InheritedEnvironment(name)),
        None => Ok(()),
    }
}

/// Fail closed on any inherited variable that could redirect GStreamer, GIO,
/// or the stream transport away from the package under test.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn reject_inherited_probe_environment(
    keys: impl IntoIterator<Item = OsString>,
) -> Result<(), PlatformRuntimeError> {
    let mut forbidden = keys
        .into_iter()
        .filter(|key| forbidden_probe_environment_key(key))
        .map(|key| key.to_string_lossy().to_ascii_uppercase())
        .collect::<Vec<_>>();
    forbidden.sort();
    match forbidden.into_iter().next() {
        Some(key) => Err(PlatformRuntimeError::InheritedEnvironment(key)),
        None => Ok(()),
    }
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
/// Whether an inherited variable could redirect GStreamer, GIO, or the transport.
fn forbidden_probe_environment_key(key: &OsStr) -> bool {
    let normalized = key.to_string_lossy().to_ascii_uppercase();
    (normalized.starts_with("GST_") && normalized != REGISTRY_ENVIRONMENT_KEY)
        || normalized == "GIO_EXTRA_MODULES"
        || normalized == "GIO_USE_PROXY_RESOLVER"
        || matches!(
            normalized.as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
        )
}

/// Require the helper-supplied registry to be an absolute file path inside
/// the fresh cache root, so the probe can only ever build a new registry.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn probe_registry_path(
    value: Option<&OsStr>,
    cache_root: &Path,
) -> Result<PathBuf, PlatformRuntimeError> {
    let value = value.filter(|value| !value.is_empty());
    let registry = PathBuf::from(value.ok_or(PlatformRuntimeError::RegistryEnvironment)?);
    if !registry.is_absolute() || has_relative_components(&registry) {
        return Err(PlatformRuntimeError::RegistryEnvironment);
    }
    let projected = resolve_existing_prefix(&registry)
        .map_err(|_| PlatformRuntimeError::RegistryEnvironment)?;
    if projected == cache_root || !projected.starts_with(cache_root) {
        return Err(PlatformRuntimeError::RegistryEnvironment);
    }
    Ok(registry)
}

#[cfg_attr(
    not(any(test, target_os = "windows", target_os = "macos")),
    allow(dead_code)
)]
/// Require the fresh registry to be a non-empty file outside the package.
fn verify_probe_registry(registry: &Path, install_root: &Path) -> Result<(), PlatformRuntimeError> {
    let metadata =
        std::fs::metadata(registry).map_err(|_| PlatformRuntimeError::RegistryMissing)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(PlatformRuntimeError::RegistryMissing);
    }
    let resolved_registry = registry
        .canonicalize()
        .map_err(|_| PlatformRuntimeError::RegistryMissing)?;
    let resolved_install = install_root.canonicalize().map_err(|_| {
        #[cfg(target_os = "macos")]
        {
            PlatformRuntimeError::MacBundleLayout
        }
        #[cfg(not(target_os = "macos"))]
        {
            PlatformRuntimeError::PackageLayout
        }
    })?;
    if resolved_registry.starts_with(resolved_install) {
        return Err(PlatformRuntimeError::RegistryInsideInstall);
    }
    Ok(())
}

/// Spawn the bundled scanner once with no arguments and require its
/// documented no-argument exit status, proving the helper starts and finds
/// the packaged DLLs the way GStreamer will start it.
#[cfg(target_os = "windows")]
fn preflight_windows_plugin_scanner(
    scanner: &Path,
    bin_dir: &Path,
) -> Result<(), PlatformRuntimeError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // GStreamer prepends its own DLL directory to the scanner's PATH; do the
    // same for the preflight so the two launches resolve the same DLLs.
    let mut path = bin_dir.as_os_str().to_owned();
    if let Some(inherited) = env::var_os("PATH") {
        path.push(";");
        path.push(inherited);
    }
    let mut child = Command::new(scanner)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PlatformRuntimeError::ScannerStart)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if scanner_no_args_exit_code_is_expected(status.code()) {
                    return Ok(());
                }
                return Err(PlatformRuntimeError::ScannerStatus);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_windows_scanner(&mut child)?;
                return Err(PlatformRuntimeError::ScannerDeadline);
            }
            Err(_) => {
                terminate_windows_scanner(&mut child)?;
                return Err(PlatformRuntimeError::ScannerStatus);
            }
        }
    }
}

#[cfg(target_os = "windows")]
/// Kill and reap a scanner that overran its bound.
fn terminate_windows_scanner(child: &mut std::process::Child) -> Result<(), PlatformRuntimeError> {
    let killed = child.kill().is_ok();
    let waited = child.wait().is_ok();
    if !killed || !waited {
        return Err(PlatformRuntimeError::ScannerTermination);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn preflight_macos_plugin_scanner(scanner: &Path) -> Result<(), PlatformRuntimeError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new(scanner)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PlatformRuntimeError::ScannerStart)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if scanner_no_args_exit_code_is_expected(status.code()) {
                    return Ok(());
                }
                return Err(PlatformRuntimeError::ScannerStatus);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_macos_scanner(&mut child)?;
                return Err(PlatformRuntimeError::ScannerDeadline);
            }
            Err(_) => {
                terminate_macos_scanner(&mut child)?;
                return Err(PlatformRuntimeError::ScannerStatus);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn terminate_macos_scanner(child: &mut std::process::Child) -> Result<(), PlatformRuntimeError> {
    let killed = child.kill().is_ok();
    let waited = child.wait().is_ok();
    if !killed || !waited {
        return Err(PlatformRuntimeError::ScannerTermination);
    }
    Ok(())
}

/// `gst-plugin-scanner` exits with status 1 when started without arguments.
#[cfg_attr(
    not(any(test, target_os = "windows", target_os = "macos")),
    allow(dead_code)
)]
fn scanner_no_args_exit_code_is_expected(code: Option<i32>) -> bool {
    code == Some(1)
}

/// Find the one hidden probe flag and its cache root, if present.
fn parse_platform_runtime_probe_request(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<PathBuf>, PlatformRuntimeError> {
    let mut args = args.into_iter().skip(1);
    let mut probe_root = None;
    while let Some(arg) = args.next() {
        if arg == PLATFORM_RUNTIME_PROBE_FLAG {
            if probe_root.is_some() {
                return Err(PlatformRuntimeError::DuplicateProbeFlag);
            }
            let root = args.next().ok_or(PlatformRuntimeError::MissingCacheRoot)?;
            if root == PLATFORM_RUNTIME_PROBE_FLAG {
                return Err(PlatformRuntimeError::DuplicateProbeFlag);
            }
            probe_root = Some(PathBuf::from(root));
        }
    }
    Ok(probe_root)
}

/// Require an absolute, fresh, empty cache root outside the package, creating
/// it when absent, and return its canonical path.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn validate_probe_cache_root(
    root: &Path,
    install_root: &Path,
) -> Result<PathBuf, PlatformRuntimeError> {
    if !root.is_absolute() {
        return Err(PlatformRuntimeError::CacheRootNotAbsolute);
    }
    if has_relative_components(root) {
        return Err(PlatformRuntimeError::CacheRootRelativeComponents);
    }
    let resolved_install = install_root
        .canonicalize()
        .map_err(|_| PlatformRuntimeError::PackageLayout)?;
    let projected_root =
        resolve_existing_prefix(root).map_err(|_| PlatformRuntimeError::CacheRootUnavailable)?;
    if projected_root.starts_with(&resolved_install) {
        return Err(PlatformRuntimeError::CacheRootInsideInstall);
    }
    if root.exists() && (!root.is_dir() || !directory_is_empty(root)?) {
        return Err(PlatformRuntimeError::CacheRootNotFresh);
    }
    std::fs::create_dir_all(root).map_err(|_| PlatformRuntimeError::CacheRootUnavailable)?;
    let resolved_root = root
        .canonicalize()
        .map_err(|_| PlatformRuntimeError::CacheRootUnavailable)?;
    if resolved_root.starts_with(&resolved_install) {
        return Err(PlatformRuntimeError::CacheRootInsideInstall);
    }
    if !directory_is_empty(&resolved_root)? {
        return Err(PlatformRuntimeError::CacheRootNotFresh);
    }
    Ok(resolved_root)
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
/// Whether a directory has no entries at all.
fn directory_is_empty(directory: &Path) -> Result<bool, PlatformRuntimeError> {
    let mut entries =
        std::fs::read_dir(directory).map_err(|_| PlatformRuntimeError::CacheRootUnavailable)?;
    Ok(entries.next().is_none())
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
/// Whether a path contains `.` or `..` components.
fn has_relative_components(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    })
}

/// Resolve the longest existing ancestor of a path and reattach the rest, so
/// a path that does not exist yet can still be checked against a prefix.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn resolve_existing_prefix(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the path has no existing ancestor",
            )
        })?;
    }
    let resolved = existing.canonicalize()?;
    let suffix = path.strip_prefix(existing).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the unresolved suffix could not be derived",
        )
    })?;
    Ok(resolved.join(suffix))
}

/// Replace a file atomically from a temporary sibling in the same directory.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn atomic_replace(path: &Path, contents: &[u8]) -> Result<(), PlatformRuntimeError> {
    use std::io::Write;

    let parent = path.parent().ok_or(PlatformRuntimeError::Sentinel)?;
    std::fs::create_dir_all(parent).map_err(|_| PlatformRuntimeError::Sentinel)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| PlatformRuntimeError::Sentinel)?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| PlatformRuntimeError::Sentinel)?;
    temporary
        .persist(path)
        .map_err(|_| PlatformRuntimeError::Sentinel)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_fake_package(root: &Path) -> PathBuf {
        let bin = root.join("Balun").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.join("Balun").join("lib").join("gstreamer-1.0")).unwrap();
        let exe = bin.join("balun.exe");
        fs::write(&exe, b"binary").unwrap();
        exe
    }

    #[test]
    fn probe_parser_accepts_one_explicit_cache_root() {
        let args = [
            OsString::from("balun"),
            OsString::from("--unrelated"),
            OsString::from(PLATFORM_RUNTIME_PROBE_FLAG),
            OsString::from("/fresh cache"),
        ];
        assert_eq!(
            parse_platform_runtime_probe_request(args).unwrap(),
            Some(PathBuf::from("/fresh cache"))
        );
        assert_eq!(
            parse_platform_runtime_probe_request([OsString::from("balun")]).unwrap(),
            None
        );
    }

    #[test]
    fn probe_parser_rejects_missing_or_duplicate_roots() {
        let flag = || OsString::from(PLATFORM_RUNTIME_PROBE_FLAG);
        assert_eq!(
            parse_platform_runtime_probe_request([OsString::from("balun"), flag()]),
            Err(PlatformRuntimeError::MissingCacheRoot)
        );
        assert_eq!(
            parse_platform_runtime_probe_request([OsString::from("balun"), flag(), flag()]),
            Err(PlatformRuntimeError::DuplicateProbeFlag)
        );
        assert_eq!(
            parse_platform_runtime_probe_request([
                OsString::from("balun"),
                flag(),
                OsString::from("/first"),
                flag(),
                OsString::from("/second"),
            ]),
            Err(PlatformRuntimeError::DuplicateProbeFlag)
        );
    }

    #[test]
    fn package_detection_requires_bin_executable_and_plugin_directory() {
        let temp = tempfile::tempdir().unwrap();
        let flat = temp.path().join("flat").join("balun.exe");
        fs::create_dir_all(flat.parent().unwrap()).unwrap();
        fs::write(&flat, b"binary").unwrap();
        fs::create_dir_all(temp.path().join("flat").join("lib").join("gstreamer-1.0")).unwrap();
        assert!(detect_windows_package(&flat).is_none());

        let bin_only = temp.path().join("partial").join("bin").join("balun.exe");
        fs::create_dir_all(bin_only.parent().unwrap()).unwrap();
        fs::write(&bin_only, b"binary").unwrap();
        assert!(detect_windows_package(&bin_only).is_none());

        let exe = write_fake_package(temp.path());
        let layout = detect_windows_package(&exe).unwrap();
        let root = temp.path().join("Balun");
        assert_eq!(layout.install_root, root);
        assert_eq!(layout.bin_dir, root.join("bin"));
        assert_eq!(layout.plugin_dir, root.join("lib").join("gstreamer-1.0"));
        assert_eq!(
            layout.scanner,
            root.join("libexec")
                .join("gstreamer-1.0")
                .join("gst-plugin-scanner.exe")
        );
    }

    #[test]
    fn probe_rejects_every_gstreamer_gio_and_proxy_key_except_the_registry() {
        for key in [
            "GST_PLUGIN_PATH",
            "gst_debug",
            "GST_PLUGIN_SYSTEM_PATH_1_0",
            "GST_REGISTRY_1_0",
            "GIO_EXTRA_MODULES",
            "gio_use_proxy_resolver",
            "HTTP_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "no_proxy",
        ] {
            assert!(forbidden_probe_environment_key(OsStr::new(key)), "{key}");
        }
        for key in [
            "GST_REGISTRY",
            "PATH",
            "GIO_MODULE_DIR",
            "GDK_PIXBUF_MODULE_FILE",
        ] {
            assert!(!forbidden_probe_environment_key(OsStr::new(key)), "{key}");
        }
        assert_eq!(
            reject_inherited_probe_environment([
                OsString::from("PATH"),
                OsString::from("gst_plugin_path"),
                OsString::from("GST_REGISTRY"),
            ]),
            Err(PlatformRuntimeError::InheritedEnvironment(
                "GST_PLUGIN_PATH".to_owned()
            ))
        );
        assert_eq!(
            reject_inherited_probe_environment([
                OsString::from("PATH"),
                OsString::from("GST_REGISTRY")
            ]),
            Ok(())
        );
    }

    #[test]
    fn scanner_preflight_accepts_only_the_documented_no_args_status() {
        assert!(scanner_no_args_exit_code_is_expected(Some(1)));
        for code in [None, Some(0), Some(2), Some(i32::MAX)] {
            assert!(!scanner_no_args_exit_code_is_expected(code));
        }
    }

    #[test]
    fn probe_cache_root_must_be_absolute_fresh_empty_and_external() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Balun");
        fs::create_dir(&install).unwrap();
        let cache = temp.path().join("Fresh Cache With Spaces");

        let resolved = validate_probe_cache_root(&cache, &install).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.is_dir());
        fs::write(cache.join("stale-registry.bin"), b"stale").unwrap();
        assert_eq!(
            validate_probe_cache_root(&cache, &install),
            Err(PlatformRuntimeError::CacheRootNotFresh)
        );

        let inside = install.join("probe-cache");
        assert_eq!(
            validate_probe_cache_root(&inside, &install),
            Err(PlatformRuntimeError::CacheRootInsideInstall)
        );
        assert!(!inside.exists());
        assert_eq!(
            validate_probe_cache_root(Path::new("relative-cache"), &install),
            Err(PlatformRuntimeError::CacheRootNotAbsolute)
        );
        let dotted = temp.path().join("..").join("dotted");
        assert_eq!(
            validate_probe_cache_root(&dotted, &install),
            Err(PlatformRuntimeError::CacheRootRelativeComponents)
        );
    }

    #[test]
    fn registry_environment_must_name_a_file_inside_the_cache_root() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let cache = cache.canonicalize().unwrap();
        let registry = cache.join("gstreamer").join("registry.bin");
        assert_eq!(
            probe_registry_path(Some(registry.as_os_str()), &cache),
            Ok(registry.clone())
        );
        for rejected in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative/registry.bin")),
            Some(cache.as_os_str().to_owned()),
            Some(temp.path().join("elsewhere.bin").into_os_string()),
            Some(cache.join("..").join("escape.bin").into_os_string()),
        ] {
            assert_eq!(
                probe_registry_path(rejected.as_deref(), &cache),
                Err(PlatformRuntimeError::RegistryEnvironment)
            );
        }
    }

    #[test]
    fn registry_verification_requires_a_nonempty_external_file() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Balun");
        fs::create_dir(&install).unwrap();
        let registry = temp.path().join("registry.bin");
        assert_eq!(
            verify_probe_registry(&registry, &install),
            Err(PlatformRuntimeError::RegistryMissing)
        );
        fs::write(&registry, b"").unwrap();
        assert_eq!(
            verify_probe_registry(&registry, &install),
            Err(PlatformRuntimeError::RegistryMissing)
        );
        fs::write(&registry, b"registry").unwrap();
        assert_eq!(verify_probe_registry(&registry, &install), Ok(()));
        let inside = install.join("registry.bin");
        fs::write(&inside, b"registry").unwrap();
        assert_eq!(
            verify_probe_registry(&inside, &install),
            Err(PlatformRuntimeError::RegistryInsideInstall)
        );
    }

    #[test]
    fn atomic_replace_replaces_from_the_same_directory_and_never_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        let sentinel = temp.path().join("nested").join(WINDOWS_PROBE_SENTINEL_NAME);
        atomic_replace(&sentinel, b"old").unwrap();
        atomic_replace(&sentinel, WINDOWS_PROBE_SENTINEL).unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), WINDOWS_PROBE_SENTINEL);
        assert_eq!(fs::read_dir(sentinel.parent().unwrap()).unwrap().count(), 1);

        let blocker = temp.path().join("not-a-directory");
        fs::write(&blocker, b"file").unwrap();
        assert_eq!(
            atomic_replace(&blocker.join("sentinel"), b"x"),
            Err(PlatformRuntimeError::Sentinel)
        );
    }

    #[test]
    fn errors_are_fixed_and_path_free() {
        assert_eq!(
            PlatformRuntimeError::InheritedEnvironment("GST_PLUGIN_PATH".to_owned()).to_string(),
            "the platform runtime probe requires inherited GST_PLUGIN_PATH to be unset"
        );
        assert_eq!(
            PlatformRuntimeError::PackageLayout.to_string(),
            "the platform runtime probe requires the self-contained Windows package layout"
        );
        assert_eq!(WINDOWS_PROBE_SENTINEL, b"balun-windows-runtime-probe-v1\n");
    }

    #[test]
    fn macos_sentinel_and_error_definitions() {
        assert_eq!(MACOS_PROBE_SENTINEL, b"balun-macos-runtime-probe-v1\n");
        assert_eq!(MACOS_PROBE_SENTINEL_NAME, "balun-platform-runtime-probe.ok");
        assert_eq!(
            PlatformRuntimeError::MacBundleLayout.to_string(),
            "the platform runtime probe requires the self-contained macOS bundle layout"
        );
    }

    fn write_fake_mac_app(root: &Path) -> PathBuf {
        let contents = root.join("Balun.app").join("Contents");
        let macos = contents.join("MacOS");
        let resources = contents.join("Resources");
        let plugins = resources.join("lib").join("gstreamer-1.0");
        fs::create_dir_all(&macos).unwrap();
        fs::create_dir_all(&plugins).unwrap();
        fs::write(
            contents.join("Info.plist"),
            "<plist><dict><key>CFBundleExecutable</key><string>Balun</string>\
             <key>CFBundlePackageType</key><string>APPL</string></dict></plist>",
        )
        .unwrap();
        let exe = macos.join("Balun-bin");
        fs::write(&exe, b"binary").unwrap();
        let scanner = macos.join("gst-plugin-scanner");
        fs::write(&scanner, b"scanner").unwrap();
        exe
    }

    #[test]
    fn mac_bundle_detection_requires_complete_exact_shape() {
        let temp = tempfile::tempdir().unwrap();
        let exe = write_fake_mac_app(temp.path());
        let layout = detect_macos_bundle(&exe).expect("bundle should be detected");
        assert_eq!(layout.app_root, temp.path().join("Balun.app"));
        assert_eq!(
            layout.plugin_dir,
            temp.path()
                .join("Balun.app/Contents/Resources/lib/gstreamer-1.0")
        );
        assert_eq!(
            layout.scanner,
            temp.path()
                .join("Balun.app/Contents/MacOS/gst-plugin-scanner")
        );

        fs::remove_file(temp.path().join("Balun.app/Contents/Info.plist")).unwrap();
        assert!(detect_macos_bundle(&exe).is_none());
    }

    #[test]
    fn mac_suffix_only_and_false_app_shapes_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let fake = temp.path().join("Fake.app/NotContents/MacOS");
        fs::create_dir_all(&fake).unwrap();
        let exe = fake.join("Balun-bin");
        fs::write(&exe, b"binary").unwrap();
        assert!(detect_macos_bundle(&exe).is_none());

        let exe = write_fake_mac_app(temp.path());
        fs::write(
            temp.path().join("Balun.app/Contents/Info.plist"),
            "<plist><dict><key>CFBundleExecutable</key><string>Balun</string></dict></plist>",
        )
        .unwrap();
        assert!(detect_macos_bundle(&exe).is_none());
    }

    #[test]
    fn cache_paths_are_user_scoped_and_never_install_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let cache_base = temp.path().join("Library/Caches");
        let install_root = temp.path().join("Applications/Balun.app");
        let paths = runtime_cache_paths(&cache_base, "macos", "aarch64", &install_root).unwrap();
        assert!(
            paths
                .root
                .starts_with(cache_base.join("balun").join("runtime"))
        );
        assert!(!paths.gst_registry.starts_with(&install_root));
        assert!(!paths.pixbuf_loaders.starts_with(&install_root));
        assert_eq!(
            paths.gst_registry.file_name(),
            Some(std::ffi::OsStr::new("registry.bin"))
        );
        assert_eq!(
            paths.pixbuf_loaders.file_name(),
            Some(std::ffi::OsStr::new("loaders.cache"))
        );
    }

    #[test]
    fn cache_paths_separate_platform_architecture_and_install_path() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        let first_install = temp.path().join("A/Balun.app");
        let moved_install = temp.path().join("B/Balun.app");
        let mac_arm = runtime_cache_paths(&base, "macos", "aarch64", &first_install).unwrap();
        let mac_x64 = runtime_cache_paths(&base, "macos", "x86_64", &first_install).unwrap();
        let win_arm = runtime_cache_paths(&base, "windows", "aarch64", &first_install).unwrap();
        let moved = runtime_cache_paths(&base, "macos", "aarch64", &moved_install).unwrap();
        assert_ne!(mac_arm.root, mac_x64.root);
        assert_ne!(mac_arm.root, win_arm.root);
        assert_ne!(mac_arm.root, moved.root);
        assert_eq!(
            runtime_cache_paths(
                &base.join("../redirect"),
                "macos",
                "aarch64",
                &first_install
            ),
            Err(PlatformRuntimeError::CacheRootRelativeComponents)
        );
    }

    #[test]
    fn bundle_contains_mutable_cache_checks_all_known_locations() {
        let temp = tempfile::tempdir().unwrap();
        let exe = write_fake_mac_app(temp.path());
        let layout = detect_macos_bundle(&exe).unwrap();
        assert!(!bundle_contains_mutable_cache(&layout));

        let gst_reg = layout.macos_dir.join("gst-registry.bin");
        fs::write(&gst_reg, b"cache").unwrap();
        assert!(bundle_contains_mutable_cache(&layout));
        fs::remove_file(&gst_reg).unwrap();

        let pixbuf_cache_dir = layout.resources_dir.join("lib/gdk-pixbuf-2.0/2.10.0");
        fs::create_dir_all(&pixbuf_cache_dir).unwrap();
        let pixbuf_cache = pixbuf_cache_dir.join("loaders.cache");
        fs::write(&pixbuf_cache, b"cache").unwrap();
        assert!(bundle_contains_mutable_cache(&layout));
        fs::remove_file(&pixbuf_cache).unwrap();

        assert!(!bundle_contains_mutable_cache(&layout));
    }

    #[test]
    fn stable_path_fingerprint_matches_deterministic_vector() {
        let hash = stable_path_fingerprint(Path::new("/Applications/Balun.app"));
        assert_eq!(hash, 0x9692_5b5f_60eb_b3db);
        assert_eq!(format!("{hash:016x}"), "96925b5f60ebb3db");
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn probe_is_unsupported_outside_the_supported_packages() {
        assert_eq!(
            configure_for_platform(Some(Path::new("/tmp/cache"))),
            Err(PlatformRuntimeError::ProbeUnsupported)
        );
        assert_eq!(
            configure_for_platform(None),
            Ok(ToolkitPreparation::Continue)
        );
    }
}
