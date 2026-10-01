# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# actors — the classical actor framework: a scheduler, typed refs, and `on_message`.
#
# One of the two application frameworks this library ships. It REQUIRES `toolkit` because every
# actor's loop is a `spire::Task`, and nothing else: it deliberately does **not** require `ramen`,
# because the two frameworks are alternatives rather than layers, and a component that needed both
# would be a component that could not be used under either.
#
# What is here and what is next door is C++'s split rather than a choice: the message-typed things
# (`Actor<Message>`, `ActorRef<Message>`, `spawn`) must be templates and so live in `include/`, while
# the queue, the task and the type-erased entry are ordinary code in `src/scheduler.cpp`. That also
# means `idf.py build` compiles this component rather than merely registering its include path.

set(SPIRE_COMPONENT_KIND library)

idf_component_register(
    SRCS "src/scheduler.cpp"
    INCLUDE_DIRS "include"
    REQUIRES toolkit
)
