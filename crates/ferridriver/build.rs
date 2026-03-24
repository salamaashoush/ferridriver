fn main() {
    #[cfg(target_os = "macos")]
    {
        let out_dir = std::env::var("OUT_DIR").unwrap();

        // Compile host.m and host_main.c into object files via the cc crate.
        // We use cc for correct compiler detection, flag handling, and
        // cross-compilation support.

        // host.m -> host.o
        cc::Build::new()
            .file("src/backend/webkit/host.m")
            .flag("-fobjc-arc")
            .flag("-fmodules")
            .flag("-Wno-deprecated-declarations")
            .cargo_warnings(false)
            .compile("webkit_host_obj");

        // host_main.c -> host_main.o
        cc::Build::new()
            .file("src/backend/webkit/host_main.c")
            .cargo_warnings(false)
            .compile("webkit_host_main_obj");

        // Link the two object files into a standalone executable.
        // We use the cc crate's detected compiler for the final link step.
        let host_obj = format!("{out_dir}/libwebkit_host_obj.a");
        let main_obj = format!("{out_dir}/libwebkit_host_main_obj.a");
        let host_bin = format!("{out_dir}/fd_webkit_host");

        let tool = cc::Build::new().get_compiler();
        let status = tool.to_command()
            .args([&host_obj, &main_obj])
            .arg("-o")
            .arg(&host_bin)
            .args(["-framework", "Cocoa"])
            .args(["-framework", "WebKit"])
            .args(["-framework", "CoreFoundation"])
            .status()
            .expect("Failed to link webkit host binary");

        assert!(status.success(), "Failed to link webkit host binary");

        // Don't link the static libs into the Rust library -- they're only
        // used to produce the standalone binary above. Clear the link flags
        // that cc::Build emits by default.
        // (The Rust library doesn't call fd_webkit_host_main anymore.)

        println!("cargo:rerun-if-changed=src/backend/webkit/host.m");
        println!("cargo:rerun-if-changed=src/backend/webkit/host_main.c");
    }
}
