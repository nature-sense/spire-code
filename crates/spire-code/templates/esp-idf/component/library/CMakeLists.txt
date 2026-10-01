# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# __NAME__ — a pure library component: no device, no bus, no board.
#
# There is no `REQUIRES` here, and that is deliberate rather than an omission: this component names no
# IDF type. It is code — an algorithm, a filter, a codec, a framework of plain functions — and code
# that reaches for a driver is code that stopped being usable off the chip.

# What this component **is**, stated rather than guessed. The same rule the project follows with
# `SPIRE_PROJECT_STRUCTURE`: nothing else in the tree says which kind of component this is.
set(SPIRE_COMPONENT_KIND library)

idf_component_register(
    SRCS "src/__IDENT__.cpp"
    INCLUDE_DIRS "include"
)
