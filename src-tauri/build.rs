fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let proto = "../protobuf/axonkey_service.proto";
        println!("cargo:rerun-if-changed={proto}");
        let protoc = protoc_bin_vendored::protoc_bin_path().expect("bundled protoc");
        prost_build::Config::new()
            .protoc_executable(protoc)
            .compile_protos(&[proto], &["../protobuf"])
            .expect("generate AxonkeyService protobuf types");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/macos_input.m")
            .file("native/macos_audio.m")
            .flag("-fobjc-arc")
            .compile("axonkey_macos_native");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=CoreBluetooth");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=IOKit");
        println!("cargo:rerun-if-changed=native/macos_audio.m");
        println!("cargo:rerun-if-changed=native/macos_pcm_queue.h");
        println!("cargo:rerun-if-changed=native/macos_input.m");
    }
    tauri_build::build()
}
