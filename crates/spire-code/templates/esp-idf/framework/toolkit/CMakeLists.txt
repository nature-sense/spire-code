# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# toolkit — the task seam: `spire::Task`, and nothing else.
#
# Both application frameworks stand on this. A dataflow actor's loop and a mailbox actor's loop are
# both "a function running on a task of its own with a known stack", and that is the whole of what
# they share. It therefore REQUIRES nothing — in particular it does not require `ramen`: the moment it
# depended on one framework, the other would have to depend on it too.
#
# The implementation is in `src/task.cpp` (every call in it is a FreeRTOS one, so it is not a
# template and has no business in a header), which also means `idf.py build` compiles this component
# rather than merely registering its include path.

set(SPIRE_COMPONENT_KIND library)

idf_component_register(
    SRCS "src/task.cpp"
    INCLUDE_DIRS "include"
)
