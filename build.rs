//! Attach desktop platform resources and capture the macOS development icon path.
//!
//! On a Windows host this generates the icon and version resource from
//! `data/balun.ico` and the Cargo package metadata, then links it into the
//! desktop binary only. Other hosts, and the GTK-free diagnostic, get no
//! resource. The Windows packaging helper later reopens the built executable
//! and requires exactly this resource set, so a build that silently lost it
//! cannot reach a package.

/// Prepare platform resources without adding dependencies to the diagnostic.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=data/balun.ico");

    #[cfg(target_os = "macos")]
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        // GTK's native About/Quit labels read NSBundle's CFBundleName, then
        // fall back to the executable name. Embed identity in the bare Mach-O
        // too, so cargo run and build-macos.sh --run display "Balun".
        let output_dir =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
        let plist = output_dir.join("balun-info.plist");
        std::fs::write(
            &plist,
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<plist version=\"1.0\"><dict>\n",
                "<key>CFBundleName</key><string>Balun</string>\n",
                "<key>CFBundleIdentifier</key><string>io.github.jm2.Balun</string>\n",
                "</dict></plist>\n",
            ),
        )
        .expect("write macOS executable identity");
        println!(
            "cargo:rustc-link-arg-bin=balun=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );

        println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
        if let Ok(output) = std::process::Command::new("pkg-config")
            .args(["--path", "adwaita-icon-theme"])
            .output()
            && output.status.success()
        {
            let pc = String::from_utf8_lossy(&output.stdout);
            let pc = std::path::Path::new(pc.trim());
            if let Some(share) = pc.parent().and_then(std::path::Path::parent) {
                println!("cargo:rerun-if-changed={}", pc.display());
                println!(
                    "cargo:rustc-env=BALUN_BUILD_ICON_DIR={}",
                    share.join("icons").display()
                );
            }
        }
    }

    // `winresource::compile()` would emit a package-wide `rustc-link-lib`,
    // which Cargo routes only to this package's library now that
    // `src/lib.rs` exists; the resource would therefore be absent from
    // `balun.exe`. `embed_resource::compile_for()` emits the bin-scoped
    // linker directive a mixed library/binary package needs.
    #[cfg(target_os = "windows")]
    {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"),
        );
        let output_dir =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
        let resource_file = output_dir.join("balun-resource.rc");
        let icon = manifest_dir.join("data").join("balun.ico");

        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(
            icon.to_str()
                .expect("Windows resource icon path is not valid UTF-8"),
        );
        resource.set("ProductName", "Balun");
        resource.set("FileDescription", "Balun");
        resource.set("LegalCopyright", "Copyright © 2026 Balun Contributors");
        resource
            .write_resource_file(&resource_file)
            .expect("Failed to generate Windows resources");

        embed_resource::compile_for(&resource_file, ["balun"], embed_resource::NONE)
            .manifest_required()
            .expect("Failed to compile Windows resources for balun.exe");
    }
}
