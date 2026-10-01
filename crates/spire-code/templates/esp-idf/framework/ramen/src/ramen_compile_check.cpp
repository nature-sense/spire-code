// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// A translation unit that exists so `idf.py build` actually compiles the vendored header.
//
// RAMEN is header-only, so without this the component would register an include path and compile
// nothing — a chip build that passes whether or not the header is usable. This is a **compile
// check**: it proves the header parses with the chip's own compiler, at the standard the library
// states, and it claims nothing about behaviour. Behaviour is what the host test beside it is for,
// and that runs in a second instead of a minute.

#include <ramen.hpp>

#if __cplusplus < 202002L
#error "ramen requires C++20: set the standard in the project's CMakeLists.txt"
#endif

#include <type_traits>

namespace {

/// A port pair, instantiated. `sizeof` on a class type instantiates it, which is the point: parsing a
/// template is not compiling it, and a header that only parses is a header that fails the moment a
/// component wires anything up.
static_assert(sizeof(ramen::Pusher<int>) > 0, "the event half of a port");
static_assert(sizeof(ramen::Pushable<int>) > 0, "the behaviour half of a port");
static_assert(sizeof(ramen::Pullable<int>) > 0);
static_assert(sizeof(ramen::Puller<int>) > 0);
static_assert(!std::is_copy_constructible_v<ramen::Pusher<int>>, "ports are not copyable");

}  // namespace
