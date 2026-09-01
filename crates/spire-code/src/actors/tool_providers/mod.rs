// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Composer-side tool registration (the generic `ToolRouterActor` + registry
//! live in `spire-core::actors::tool_providers`).

pub mod builder;
pub use builder::build_default_registry;
