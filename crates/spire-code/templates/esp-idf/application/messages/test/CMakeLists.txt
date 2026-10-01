# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The shared types' **host test build** — a component of declarations, compiled and run on this machine.
#
# There is no framework and no fake here, and that is the point of the file: `messages/` REQUIRES nothing, so
# its test needs nothing. What is built is the header twice — once through the compile check the component's
# manifest also compiles, and once through the cases beside it.
#
#   cmake -S <this dir> -B <this dir>/build && cmake --build <this dir>/build && ctest --test-dir <this dir>/build

cmake_minimum_required(VERSION 3.16)
project(messages_test CXX)

# C++20: the library's standard, so a harness in this tree is built the way a chip build is
# (ESP-IDF compiles C++ at gnu++2b).
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

enable_testing()

add_executable(messages_test
    messages_test.cpp
    ../src/messages_compile_check.cpp
)

target_include_directories(messages_test PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../include)

target_compile_options(messages_test PRIVATE -Wall -Wextra)

add_test(NAME messages COMMAND messages_test)
