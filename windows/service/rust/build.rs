use std::{env, path::PathBuf};

fn main() {
    let target = env::var("TARGET").expect("Cargo target");
    assert_eq!(
        target, "x86_64-pc-windows-msvc",
        "AxonkeyService requires Windows x64 MSVC"
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest"));
    let protobuf = root.join("../../../protobuf");
    let schema = protobuf.join("axonkey_service.proto");
    prost_build::Config::new()
        .protoc_executable(protoc_bin_vendored::protoc_bin_path().expect("bundled protoc"))
        .compile_protos(&[&schema], &[&protobuf])
        .expect("generate service protocol");
    println!("cargo:rerun-if-changed={}", schema.display());
    let headers = root.join("../../driver/shared/include");
    cc::Build::new()
        .flag("/utf-8")
        .include(&headers)
        .file(root.join("native/boundary.c"))
        .warnings_into_errors(true)
        .compile("axonkey_boundary");
    println!("cargo:rerun-if-changed=native/boundary.c");
    println!("cargo:rerun-if-changed={}", headers.display());
}
