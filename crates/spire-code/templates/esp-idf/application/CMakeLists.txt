# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# An **ESP-IDF application** — the only `project()` in the tree, and what `idf.py build` reads.
#
# This is where a product lives: `main/` holds the components it uses, the loops it starts, and the
# board facts (pins, bus, addresses) that wire them together. None of that is written yet — an
# application is scaffolded blank, because the shape of it belongs to the library it is built
# against, and that library says how it is meant to be used in its hints.

cmake_minimum_required(VERSION 3.16)

# The project type, **stated rather than guessed**.
set(SPIRE_PROJECT_STRUCTURE idf_application)

__FRAMEWORK_BLOCK__
__LIBRARY_BLOCK__
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(spire-idf-application)
