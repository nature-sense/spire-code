// build.rs — Invokes flatc (system binary) to generate Rust bindings from the FlatBuffers schema.
//
// Prerequisites: flatc must be on PATH (install via `brew install flatbuffers`).
// Generated files are written to src/generated/ and should be gitignored.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let schema_dir = manifest_dir.join("../../schema");
    let out_dir = manifest_dir.join("src/generated");

    // Create output directory
    std::fs::create_dir_all(&out_dir).ok();

    // Locate all .fbs files
    let schema_files: Vec<_> = std::fs::read_dir(&schema_dir)
        .expect("schema directory not found")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "fbs"))
        .map(|e| e.path())
        .collect();

    if schema_files.is_empty() {
        println!(
            "cargo:warning=No .fbs schema files found in {:?}",
            schema_dir
        );
        return;
    }

    // Check if flatc is available
    let flatc_check = Command::new("flatc").arg("--version").output();

    match flatc_check {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            println!("flatc available: {}", version.trim());
        }
        _ => {
            println!("cargo:warning=flatc not found on PATH — install via `brew install flatbuffers`. Skipping codegen.");
            println!("cargo:warning=To generate code manually, run: ./build/generate-schema.sh");
            return;
        }
    }

    // Run flatc for Rust codegen
    let mut cmd = Command::new("flatc");
    cmd.args(["--rust", "-o"])
        .arg(out_dir.to_str().unwrap())
        .args(schema_files.iter().map(|p| p.to_str().unwrap()));

    let status = cmd.status().expect("flatc execution failed");
    assert!(status.success(), "flatc codegen failed — see errors above");

    // Rerun build if schemas change
    for path in &schema_files {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    println!("cargo:rerun-if-changed=build.rs");
}
