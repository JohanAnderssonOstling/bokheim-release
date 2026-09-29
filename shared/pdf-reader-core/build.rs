//! Validate prepared inputs only. Acquisition belongs to shared/pdfium/prepare.py.
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let manifest_path = root.join("shared/pdfium/artifacts.toml");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PDFIUM_PREPARED_DIR");
    println!("cargo:rerun-if-env-changed=PDFIUM_PACKAGED_PATH");
    println!("cargo:rerun-if-env-changed=PDFIUM_STATIC_LIB_PATH");
    println!("cargo:rerun-if-env-changed=PDFIUM_STATIC_LIB_PATH_x86_64_pc_windows_msvc");
    let target = env::var("TARGET").unwrap();
    let manifest: toml::Value = fs::read_to_string(&manifest_path).unwrap().parse().unwrap();
    let spec = manifest["targets"].get(&target).unwrap_or_else(|| panic!("Unsupported PDFium target: {target}"));
    let directory = env::var_os("PDFIUM_PREPARED_DIR").map(PathBuf::from).unwrap_or_else(|| root.join(".pdfium").join(&target));
    let missing = || format!("PDFium inputs are missing or stale. Run: python3 shared/pdfium/prepare.py --target {target}");
    let receipt_path = directory.join("receipt.json");
    println!("cargo:rerun-if-changed={}", receipt_path.display());
    let receipt: serde_json::Value = serde_json::from_str(&fs::read_to_string(&receipt_path).expect(&missing())).expect("invalid PDFium receipt");
    assert_eq!(receipt["target"].as_str(), Some(target.as_str()), "{}", missing());
    let manifest_hash = format!("{:x}", Sha256::digest(fs::read(&manifest_path).unwrap()));
    assert_eq!(receipt["manifest_sha256"].as_str(), Some(manifest_hash.as_str()), "{}", missing());
    assert_eq!(receipt["archive_sha256"].as_str(), spec["sha256"].as_str(), "{}", missing());
    for name in spec["files"].as_table().unwrap().values().map(|v| v.as_str().unwrap()).chain(["LICENSE", "VERSION"]) {
        let path = directory.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(&path).expect(&missing());
        let actual = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(receipt["files"][name].as_str(), Some(actual.as_str()), "PDFium prepared file checksum mismatch: {}", path.display());
    }
    println!("cargo:rustc-env=PDFIUM_NOTICES_PATH={}", directory.join("LICENSE").canonicalize().unwrap().display());
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }
    if spec.get("linkage").and_then(toml::Value::as_str) == Some("static") {
        assert!(env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default().split(',').any(|f| f == "crt-static"),
            "Windows PDFium requires the static C runtime (/MT)");
        let linked_directory = env::var_os("PDFIUM_STATIC_LIB_PATH_x86_64_pc_windows_msvc")
            .or_else(|| env::var_os("PDFIUM_STATIC_LIB_PATH"))
            .map(PathBuf::from).expect("static PDFium link directory must be configured");
        assert_eq!(directory.canonicalize().unwrap(), linked_directory.canonicalize().unwrap(),
            "PDFIUM_PREPARED_DIR and the static link directory must refer to the same verified inputs");
        let library = directory.join("pdfium.lib");
        assert_eq!(format!("{:x}", Sha256::digest(fs::read(&library).unwrap())),
            spec["library_sha256"].as_str().unwrap(), "PDFium static library checksum mismatch");
        return;
    }
    let name = spec["files"].as_table().unwrap().values().next().unwrap().as_str().unwrap();
    println!("cargo:rustc-env=PDFIUM_LIBRARY_NAME={name}");
    println!("cargo:rustc-env=PDFIUM_BUILD_LIBRARY_PATH={}", directory.join(name).canonicalize().unwrap().display());
    println!("cargo:rustc-env=PDFIUM_PACKAGED_PATH={}", env::var("PDFIUM_PACKAGED_PATH").unwrap_or_else(|_| "/app/lib/libpdfium.so".into()));
}
