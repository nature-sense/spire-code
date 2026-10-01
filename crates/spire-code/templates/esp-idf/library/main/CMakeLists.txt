# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The build harness — **not** an application.
#
# ESP-IDF requires a `main` component, and this one exists so that `idf.py build` compiles the
# components above. A library whose components are never compiled is a library whose components do
# not compile.
#
# It starts nothing, wires nothing and names no device. Keep it that way: the first actor that lands
# in this file is the signal that a library has quietly become an application.

idf_component_register(
    SRCS "build_harness.cpp"
    INCLUDE_DIRS "."
)
