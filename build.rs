//! Attach the Windows shell identity to `balun.exe`.
//!
//! On a Windows host this generates the icon and version resource from
//! `data/balun.ico` and the Cargo package metadata, then links it into the
//! desktop binary only. Other hosts, and the GTK-free diagnostic, get no
//! resource. The Windows packaging helper later reopens the built executable
//! and requires exactly this resource set, so a build that silently lost it
//! cannot reach a package.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=data/balun.ico");

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
