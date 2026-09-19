// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! `spire-embedded` — the on-device side of Spire: **the actor system, and a library of
//! peripherals**.
//!
//! # There is no hardware abstraction layer in this crate
//!
//! The peripheral interface is [`embedded_hal`]'s: a GPIO is a `digital::OutputPin`, a delay is a
//! `delay::DelayNs`, a bus is an `spi::SpiBus` or an `i2c::I2c`. The HAL that implements those
//! traits for a particular chip is the **vendor's** crate — `esp-idf-hal`, `rp2040-hal`,
//! `stm32-hal` — and we author neither. So there is no contract here to implement, nothing to keep
//! in step with a vendor release, and no trait of ours that a driver crate could be incompatible
//! with. (An earlier iteration defined `hal::{Led, DelayMs}` and an error type: that was a
//! re-invention of `OutputPin`/`DelayNs` standing between the project and the entire driver
//! ecosystem, and it is gone.)
//!
//! What is left is exactly the two things this crate is:
//!
//! 1. **The actor system** ([`actor`]). A firmware unit is a `Message` type plus a `handle`,
//!    deliberately the same shape as the host's `spire_actor::Actor`, which is what lets the drift
//!    measure read firmware the way it reads a C++ HAL implementation — a message with no handler
//!    is a missing implementation. The runtime is injected through [`actor::Spawner`], so an
//!    executor can swap a task and a queue underneath and nothing above it moves. Two runtimes ship
//!    here: a threaded one behind `std` ([`executor::StdSpawner`] — under esp-idf a std thread *is*
//!    a FreeRTOS task) and an async one behind `embassy` ([`embassy`] — a task loop plus a mailbox
//!    over an `embassy_sync` channel, for the `no_std` families).
//! 2. **Peripherals** ([`drivers`]). Devices — a strip, a shift register, a sensor — written
//!    against those traits, so they run on any board *and* on a host fake, with no `#[cfg(board)]`
//!    anywhere. Upstream crates remain the default for a device; these exist for the cases where
//!    there is none, or where the driver has to be actor-shaped.
//!
//! # Where a board's own facts live
//!
//! Which pin the LED is on, and whether it is active-low, is neither a peripheral trait nor an
//! actor's business: it is board knowledge, and the layer for it is a **BSP** (board support
//! package) — above the vendor HAL, below the application. Prefer an upstream BSP; generate one
//! only when none exists. Nothing in this crate knows a board, a chip, a vendor SDK or an OS.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod actor;
pub mod drivers;

#[cfg(feature = "embassy")]
pub mod embassy;

#[cfg(feature = "std")]
pub mod executor;

#[cfg(feature = "embassy")]
pub use embassy::EmbassyMailbox;

#[cfg(feature = "std")]
pub use executor::{QueueMailbox, StdSpawner, DEFAULT_MAILBOX_DEPTH};

/// The ecosystem peripheral traits, at the version this crate's peripherals were compiled against.
///
/// Re-exported rather than left to each consumer so a project names one version of `embedded-hal`:
/// two copies of it in one build are two different `OutputPin` traits, and the error is a type
/// mismatch that reads like the wrong import.
pub use embedded_hal;
