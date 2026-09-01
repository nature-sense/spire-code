// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! `spire-code` — the coding/development domain.
//!
//! Language/build-system modules (cargo, swift, python, node, go, maven,
//! gradle, cmake, make, ruby, meson), AST parsing (tree-sitter), HAL code
//! tooling, the platform registry types, the build/project/planning
//! subsystems, the coding actors (coordinator, system, build orchestrator,
//! plan orchestrator, ...), the composition root / standalone binary, and the
//! C ABI (`ffi`) exported as `libspire_code.dylib` for the Swift UI.
//! The generic actor runtime + in-process tools live in `spire-actor` and
//! `spire-core`; this crate depends on them.

pub mod actors;
pub mod build;
pub use build::{
    AstEdgeData, AstNodeData, AstParseResult, BuildModuleMessage, BuildOptions, BuildOutput,
    CargoBuildModule, CmakeBuildModule, GoBuildModule, GradleBuildModule, LanguageConfig,
    MakeBuildModule, MavenBuildModule, MesonBuildModule, ModuleCapability, NodeBuildModule,
    ParseSummary, PythonBuildModule, RubyBuildModule, SwiftBuildModule, TestOptions,
};
pub mod ffi;
pub mod subsystems;

/// The generic actor trait comes from `spire-actor`; tool metadata and the
/// shared build/platform types live in `spire-core` (`spire_core::actors`,
/// `spire_core::build_types`, `spire_core::platform`).
pub use spire_core::actors::{Actor, ToolInfo};

/// Test-only lock serializing in-process tests that read or mutate the
/// process-global `SPIRE_PLATFORM_DIR` env var (`build_manager` fixtures set
/// it; cargo/meson scaffold tests read it via `Platform::from_registry`).
/// Acquire it for the duration of any test touching `SPIRE_PLATFORM_DIR`.
#[doc(hidden)]
pub static PLATFORM_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());