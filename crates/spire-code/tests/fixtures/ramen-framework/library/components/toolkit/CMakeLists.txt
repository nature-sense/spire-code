# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The toolkit: the mechanism, and nothing device-specific.
#
# `ramen` is REQUIRED because `task.hpp` is about *running* Ramen actors — a producing actor's loop
# needs a FreeRTOS task, and this is that seam. A consumer needs none, because Ramen runs it inline
# on the producer's stack.
#
# The actors themselves are deliberately **not** here. An actor is a composition — a protocol plus
# the pins it happens to be wired to — and a composition belongs to the product that needs it, in
# `ProjectStructure::IdfApplication`. That is why this component has never heard of a device.

idf_component_register(
    INCLUDE_DIRS "include"
    REQUIRES ramen
)
