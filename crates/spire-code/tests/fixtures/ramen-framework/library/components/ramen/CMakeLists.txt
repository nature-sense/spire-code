# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# RAMEN — the dataflow actor library — as an ESP-IDF component.
#
# Vendored rather than fetched: it is one dependency-free header we own, and a build that reaches
# the network is a build that fails on a bench with no wifi. Upgrading it is replacing the file.

idf_component_register(INCLUDE_DIRS "include")
