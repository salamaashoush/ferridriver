fn main() {
    #[cfg(target_os = "macos")]
    {
        // Compile the Objective-C WebKit host subprocess implementation
        cc::Build::new()
            .file("src/backend/webkit/host.m")
            .flag("-fobjc-arc")
            .flag("-fmodules")
            .flag("-Wno-deprecated-declarations")
            .compile("webkit_host");

        // Link required frameworks and libraries
        println!("cargo:rustc-link-lib=framework=Cocoa");
        println!("cargo:rustc-link-lib=framework=WebKit");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");

        // Rebuild if the ObjC file changes
        println!("cargo:rerun-if-changed=src/backend/webkit/host.m");
    }
}
