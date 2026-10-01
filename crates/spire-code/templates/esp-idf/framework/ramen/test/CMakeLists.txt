# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# ramen's **host test** — the framework's own semantics, pinned on this machine.
#
# RAMEN is pure C++: it has no runtime, no scheduler and no FreeRTOS, so unlike the other two
# framework components it can be tested here rather than only compiled for a chip. That matters more
# for this one than for anything else in the library, because it is the piece every dataflow
# application is built on: if pushing is not inline, or a topic does not fire in link order, every
# application above it is wrong in a way no component test would catch.
#
#   cmake -S <this dir> -B <this dir>/build && cmake --build <this dir>/build && ctest --test-dir <this dir>/build

cmake_minimum_required(VERSION 3.16)
project(ramen_test CXX)

# The library's standard. Stated here as well as at the project level because this harness builds on
# the host, where there is no ESP-IDF to inherit the setting from.
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

enable_testing()

add_executable(ramen_test ramen_test.cpp)

target_include_directories(ramen_test PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../include)

target_compile_options(ramen_test PRIVATE -Wall -Wextra)

add_test(NAME ramen COMMAND ramen_test)
