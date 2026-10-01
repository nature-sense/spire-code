# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# `__IDENT__` — this __WHAT__ compiled and run on this machine, with no board, no chip and no IDF.
#
__WHY__
#
# The library is resolved the way the application's `CMakeLists.txt` resolves it, so a relative path means
# the same thing in both files — this is a cmake project of its own, so it has to be told.
#
#   cmake -S <this dir> -B <this dir>/build && cmake --build <this dir>/build && ctest --test-dir <this dir>/build

cmake_minimum_required(VERSION 3.16)
project(__IDENT___test CXX)

# C++20: the library's standard — its dataflow framework needs it — so every harness in this tree is built the
# way a chip build is (ESP-IDF compiles C++ at gnu++2b).
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

__LIBRARY_RESOLUTION__

enable_testing()

__USED_SOURCES__add_executable(__IDENT___test
    __IDENT___test.cpp
    ../src/__IDENT__.cpp
__FRAMEWORK_SOURCES____USED_SOURCES_LIST__)

# The fakes first — that order *is* the seam — then the components' own headers. `../../messages/include` is
# this application's own shared types: the unit's header includes them, and they live beside the unit rather
# than in the library.
target_include_directories(__IDENT___test PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/../include
    ${CMAKE_CURRENT_SOURCE_DIR}/../../messages/include
__TEST_DIRS____INCLUDE_DIRS__)

__THREADS__target_compile_options(__IDENT___test PRIVATE -Wall -Wextra)

add_test(NAME __IDENT__ COMMAND __IDENT___test)
