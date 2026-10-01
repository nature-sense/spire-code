# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# actors' **host test** — the framework exercised on this machine, with a fake FreeRTOS underneath.
#
# The component is built here the same way a chip builds it: its own sources (`../src/scheduler.cpp`
# and the toolkit's `task.cpp`) compiled unmodified, against `<freertos/...>` names that this
# directory's `freertos/` satisfies instead of the chip's. That path order is the trick, and it is the
# same one a driver's host test uses to put a fake bus in front of a device: `test/` comes before the
# component's own include path.
#
# `../../toolkit/include` is the sibling component this one REQUIRES. The path is the same inside a
# generated library, where it reads `components/toolkit/include`, because the framework's template
# tree mirrors the `components/` layout on purpose.
#
#   cmake -S <this dir> -B <this dir>/build && cmake --build <this dir>/build && ctest --test-dir <this dir>/build

cmake_minimum_required(VERSION 3.16)
project(actors_test CXX)

# The library's standard (see SPIRE.md): C++20, because the dataflow framework beside this one needs
# it and a library states its standard once.
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

enable_testing()

add_executable(actors_test
    actors_test.cpp
    freertos_fake.cpp
    message_tag_probe.cpp
    ../src/scheduler.cpp
    ../../toolkit/src/task.cpp
)

target_include_directories(actors_test PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/../include
    ${CMAKE_CURRENT_SOURCE_DIR}/../../toolkit/include
)

find_package(Threads REQUIRED)
target_link_libraries(actors_test PRIVATE Threads::Threads)

target_compile_options(actors_test PRIVATE -Wall -Wextra)

add_test(NAME actors COMMAND actors_test)
