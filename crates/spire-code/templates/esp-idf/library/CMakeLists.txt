# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# An **ESP-IDF component library** — the only `project()` in the tree, and what `idf.py build`
# reads.
#
# The product of this project is `components/*`, and it starts with **none of them**. The tool that
# made this project has no opinion about what belongs here: a protocol driver, a board's support
# package, a framework, or all three. What it does insist on is the boundary — an *application* is a
# separate project that depends on these components, and a library that grows an application is a
# library nobody can use twice.
#
# How the components here are meant to be used is written down in `SPIRE.md`. That file is the
# contract with whoever builds against this library; nothing else in the tree carries the
# architecture.

cmake_minimum_required(VERSION 3.16)

# The project type, **stated rather than guessed**. spire-code's reader looks for exactly this line
# to tell a library from an application; nothing else in the tree identifies the kind.
set(SPIRE_PROJECT_STRUCTURE idf_library)

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(spire-idf-library)
