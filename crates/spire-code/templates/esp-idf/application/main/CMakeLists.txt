# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The application's own component.
#
# `REQUIRES` names the components this application's sources include: a component's header resolves
# only if its component is named here, and when it is not, the build fails at the include rather than
# at this line. A design phase that ran has already named them — see the comment below.
#
# The sources are **collected** rather than listed: how the composition is divided into files is the
# model's business, and this manifest is structural, so a list of filenames here would be a list it
# could not extend — and a `.cpp` that is never compiled is a link error that names neither the file
# nor the reason. Three things this is *not*, each one found by a real `idf.py build`:
#
#  * `idf_component_register`'s own `SRC_DIRS` globs one directory and does not recurse;
#  * a `"*.cpp"` *entry* in `SRCS` is taken as a literal filename ("Cannot find source file: …/*.cpp");
#  * and `CONFIGURE_DEPENDS` cannot be used here at all — IDF includes this file in **script mode** to
#    read the component's requirements, where it is an error ("CONFIGURE_DEPENDS is invalid for script
#    and find package modes"). So a file added *after* the first configure needs a reconfigure, which
#    the fill phase, writing before any build, does not.
file(GLOB_RECURSE SPIRE_APPLICATION_SOURCES "${CMAKE_CURRENT_LIST_DIR}/*.cpp")

idf_component_register(
    SRCS ${SPIRE_APPLICATION_SOURCES}
    INCLUDE_DIRS "."
__REQUIRES__)
