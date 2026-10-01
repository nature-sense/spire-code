# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# ramen — the dataflow application framework, vendored in as a **template**.
#
# Upstream:  https://github.com/Zubax/ramen
# Revision:  94886680bd48901121350c4542116abeda1eb746 (main at the time of writing)
# sha256:    f7c6b0362173ab40b7ee7b5c77c1bd328f6a3019d51489662cf6301039a8565a
# Licence:   MIT — inherited by every library scaffolded from this template.
#
# The whole of `include/ramen.hpp` is upstream's, byte for byte, and is **upgraded by replacing it**.
# Do not edit it: an edit here is an edit to a third-party project, with nobody to upstream it to and
# nothing to carry it forward. Change the template instead, and say why.
#
# **C++20 needs nothing added here.** ESP-IDF compiles C++ for chip targets at `-std=gnu++2b`
# (`tools/cmake/build.cmake`), which is past C++20 — so RAMEN builds on a chip as it stands, and a
# project-level `-std=` would be a second, conflicting answer to a question IDF has already answered.
# What does need it is anything built **outside** IDF: the host test beside this file sets C++20
# itself, and `SPIRE.md` states the requirement for anyone building a component by hand.

set(SPIRE_COMPONENT_KIND library)

idf_component_register(
    SRCS "src/ramen_compile_check.cpp"
    INCLUDE_DIRS "include"
)
