// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

// The fake's own additions — the parts a real FreeRTOS does not have, and therefore does not have a
// header for.

/// Wait for every fake task to finish.
///
/// A chip has no equivalent because it has no need of one: a task that has been stopped is gone. A
/// host thread that has been stopped still has to be *joined*, and a `std::thread` destroyed while
/// joinable calls `std::terminate` — so the test says when, and it says so after `Scheduler::stop()`,
/// which is what guarantees the loops have already returned.
void spire_fake_join_tasks();
