// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Build subsystem — language-module router + orchestration.

pub mod build_manager;
pub use build_manager::{BuildManagerActor, BuildManagerMessage};
pub mod build_orchestrator;
pub use build_orchestrator::{BuildOrchestrator, BuildOrchestratorMessage};
