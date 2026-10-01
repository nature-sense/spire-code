// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Dev harness: re-materialize an ESP-IDF **component library** from the current template.
//!
//! Usage:
//!   cargo run -p spire-code --example rematerialize_library -- [--dry-run] [project-name] [dest-dir]
//!
//! Defaults to `spire-idf` in `~/naturesense/spire/spire-idf`, beside this repo.
//!
//! **Why this exists.** The framework is *shipped*, not generated: `library_scaffold` bakes the
//! template into the binary with `include_str!`, and the only thing that writes it to disk is the
//! scaffold path inside the JSON-RPC server. So there is no CLI to bring a library that was
//! scaffolded *earlier* back up to date with a template that has since changed — and the framework
//! changes often. This drives the **same** `library_scaffold` the server runs and writes `out.files`
//! the way `materialize_scaffold_files` does, so the bytes are the scaffold's own, never a copy.
//!
//! Two things it does deliberately that a plain "write everything" would not:
//!
//!   * a file whose content already matches is **left alone**, so its mtime — which is what cmake
//!     and `idf.py build` use to decide what to rebuild — does not move for a no-op;
//!   * nothing is ever **deleted**. A component added after the original scaffold (a driver, an
//!     algorithm) is not in `out.files` and is left exactly as it is; the scaffold's structural
//!     files are overwritten and the library's own work is not touched.
//!
//! `--dry-run` prints the same delta and writes nothing.

use std::path::PathBuf;

use spire_code::build::idf_projects::library_scaffold;

fn main() {
    let (dry_run, name, dest) = parse_args();

    let out = library_scaffold(&name, &[])
        .unwrap_or_else(|e| panic!("the library scaffold refused '{name}': {e}"));

    let (mut added, mut updated, mut current) = (0usize, 0usize, 0usize);
    for file in &out.files {
        let path = dest.join(&file.path);
        let on_disk = std::fs::read(&path).ok();
        if on_disk.as_deref() == Some(file.content.as_bytes()) {
            current += 1;
            continue;
        }
        let (verb, past) = if on_disk.is_some() {
            updated += 1;
            ("update", "updated")
        } else {
            added += 1;
            ("add", "added")
        };
        println!(
            "{} {}",
            if dry_run {
                format!("would {verb}")
            } else {
                past.to_string()
            },
            file.path
        );
        if dry_run {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a file has a parent");
        }
        std::fs::write(&path, &file.content).expect("the file writes");
    }

    println!(
        "---\n{name} -> {}: {added} added, {updated} updated, {current} already current{}",
        dest.display(),
        if dry_run {
            " (dry run: nothing written)"
        } else {
            ""
        }
    );
}

/// `[--dry-run] [project-name] [dest-dir]`, with the defaults this repo's sibling library uses.
fn parse_args() -> (bool, String, PathBuf) {
    let mut dry_run = false;
    let mut positional: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--dry-run" || arg == "-n" {
            dry_run = true;
        } else if arg == "-h" || arg == "--help" {
            println!("usage: rematerialize_library [--dry-run] [project-name] [dest-dir]");
            std::process::exit(0);
        } else {
            positional.push(arg);
        }
    }
    let name = positional
        .first()
        .cloned()
        .unwrap_or_else(|| "spire-idf".to_string());
    let dest = positional.get(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("naturesense/spire/spire-idf")
    });
    (dry_run, name, dest)
}
