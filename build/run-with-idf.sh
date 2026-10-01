#!/bin/bash
# run-with-idf.sh — launch Spire so that an **in-app ESP-IDF build** can find its toolchain.
#
#   ./build/run-with-idf.sh            launch the app with an ESP-IDF environment resolved first
#   ./build/run-with-idf.sh --check    resolve the environment, prove it works, print it, and exit
#   ./build/run-with-idf.sh -- <cmd>   run a command in that environment (e.g. `-- idf.py build`)
#
# ## Why this exists
#
# The IDF build module does **no PATH surgery** when the process it runs in already has an exported
# ESP-IDF: it runs `idf.py` and expects the environment `export.sh` sets — `IDF_PATH`, the toolchain on
# `PATH`. That is right for the product (Spire references an exported environment rather than inventing
# one), and it is also why a bundle opened with `open` used to be the one place a *chip* application's
# build failed for a reason that had nothing to do with the application. The module now fills exactly
# that gap itself: `install_for_build` resolves the install **when the process has none** and states the
# environment plus the exact `<venv>/bin/python <IDF_PATH>/tools/idf.py` to run, so a chip build works
# from a plain `open` (see §6f of `docs/esp-idf-architecture.md`).
#
# What this wrapper is for now is **choosing** — `SPIRE_IDF_EXPORT`, or a particular `export.sh` — and
# for running a command in the very environment the app will use, which is the only way to tell "the app
# cannot build" from "this machine cannot build". The app leaves an exported environment exactly as it
# finds it, so what this resolves is what the app then builds with. It sources an ESP-IDF environment
# and then execs the **binary** rather than calling `open`, so the app inherits it; running the
# executable directly is supported (see `SpireApp`'s `setActivationPolicy(.regular)`, which exists for
# exactly that).
#
# Two things it does that `export.sh` alone does not, both learned on a machine where `export.sh` is
# broken (an unsupported system `ninja` and a venv that refuses to activate):
#
#   1. if no `export.sh` works, it *synthesises* the same environment from `IDF_PATH` and
#      `IDF_TOOLS_PATH` — the venv, the toolchain directories, `ESP_ROM_ELF_DIR`;
#   2. if `idf.py` itself does not run (its shebang pointing at an interpreter without the venv), it
#      writes a one-line shim early on `PATH`, because the app runs `idf.py` **by name** and the name
#      has to resolve to something that works.
#
# Without an ESP-IDF at all it still launches, and says so: the app is perfectly usable for native
# projects, and a warning that names what it looked for is worth more than a silent start.
#
# No `set -e`: this script's whole job is to *try* environments, and most of the attempts fail by design
# (an `export.sh` that refuses, an `idf.py` that will not run). `-e` would abort on the first expected
# failure; the things that must not be silent — whether an environment was found, and whether the launch
# happened — are checked explicitly below.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_BIN="$ROOT/build/Spire.app/Contents/MacOS/SpireUI"
DEV_BIN="$ROOT/ui/swift/.build/release/SpireUI"
CHECK_ONLY=""
RUN_AFTER=""
[ "${1:-}" = "--check" ] && CHECK_ONLY="yes"
# `-- <command…>` runs a command **in the resolved environment** and exits: the same wrapper that launches
# the app is then also how a person builds or flashes by hand with the environment the app will use, which
# is the only way to tell "the app cannot build" from "this machine cannot build".
if [ "${1:-}" = "--" ]; then
    shift
    RUN_AFTER="$*"
fi

say() { printf '%s\n' "$*"; }

# ── Where an ESP-IDF environment could be ────────────────────────────────────────────────────────────
# An explicit `SPIRE_IDF_EXPORT` wins, then the IDF the shell already knows, then the two places an
# install usually lands. `~/.espressif/esp-idf/<version>/export.sh` is the versioned layout the installer
# writes, and it is the one on this machine.
candidates=""
add_candidate() {
    if [ -n "${1:-}" ]; then
        candidates="${candidates}${candidates:+
}$1"
    fi
    return 0
}
add_candidate "${SPIRE_IDF_EXPORT:-}"
if [ -n "${IDF_PATH:-}" ]; then
    add_candidate "$IDF_PATH/export.sh"
fi
add_candidate "$HOME/esp/esp-idf/export.sh"
add_candidate "/opt/esp-idf/export.sh"
for versioned in "$HOME"/.espressif/esp-idf/*/; do
    if [ -d "$versioned" ]; then
        add_candidate "${versioned}export.sh"
    fi
done

used=""
while IFS= read -r script; do
    [ -n "$script" ] || continue
    [ -f "$script" ] || continue
    # Sourced in a subshell first: `export.sh` may fail half-way through (it does here), and a failed
    # source in *this* shell would leave a broken environment behind it.
    if ( set +u; . "$script" >/dev/null 2>&1 && command -v idf.py >/dev/null 2>&1 ) 2>/dev/null; then
        set +u
        . "$script" >/dev/null 2>&1 || true
        set -u
        used="$script"
        break
    fi
done <<EOF
$candidates
EOF

# ── The same environment, synthesised, when no `export.sh` can be used ───────────────────────────────
# This is not a second environment; it is what `export.sh` writes, written by hand because the script
# itself refuses on this machine. Every path is derived from `IDF_TOOLS_PATH`, the one variable that
# says where the installer put everything.
if [ -z "$used" ]; then
    idf_path="${IDF_PATH:-}"
    if [ -z "$idf_path" ]; then
        for versioned in "$HOME"/.espressif/esp-idf/*/; do
            [ -d "$versioned" ] && idf_path="${versioned%/}" && break
        done
    fi
    if [ -n "$idf_path" ] && [ -f "$idf_path/tools/idf.py" ]; then
        export IDF_PATH="$idf_path"
        export IDF_TOOLS_PATH="${IDF_TOOLS_PATH:-$HOME/.espressif}"
        if [ -z "${IDF_PYTHON_ENV_PATH:-}" ]; then
            for env_dir in "$IDF_TOOLS_PATH"/python_env/idf*_py*_env; do
                [ -d "$env_dir" ] && export IDF_PYTHON_ENV_PATH="$env_dir" && break
            done
        fi
        for tool in \
            xtensa-esp-elf/*/xtensa-esp-elf/bin \
            riscv32-esp-elf/*/riscv32-esp-elf/bin \
            esp-clang/*/esp-clang/bin \
            ninja/* \
            cmake/*/CMake.app/Contents/bin
        do
            for dir in $IDF_TOOLS_PATH/tools/$tool; do
                [ -d "$dir" ] && PATH="$dir:$PATH"
            done
        done
        # Without this every `idf.py` configure prints a gdbinit warning (esp_rom/gen_gdbinit.py).
        for dir in "$IDF_TOOLS_PATH"/tools/esp-rom-elfs/*/; do
            [ -d "$dir" ] && export ESP_ROM_ELF_DIR="${dir%/}" && break
        done
        export PATH
        used="synthesised from IDF_PATH=$IDF_PATH and IDF_TOOLS_PATH=$IDF_TOOLS_PATH"
    fi
fi

# ── `idf.py` has to *run*, not merely exist ──────────────────────────────────────────────────────────
# A machine whose `idf.py` script has the wrong interpreter (a venv that is not on its shebang) needs the
# venv's `bin` first, or a shim in front of it: the shim is the venv's python calling the real script, and
# putting the venv first is what a working `export.sh` arranges. The app no longer depends on the name
# resolving — it resolves the install itself and runs `<venv>/bin/python <IDF_PATH>/tools/idf.py` (§6f of
# `docs/esp-idf-architecture.md`) — but what this wrapper hands it is an *exported* environment, and the
# app leaves an exported environment alone, so everything started from here (`-- <cmd>`, `--check`) still
# needs `idf.py` to work by name.
if [ -n "$used" ] && ! idf.py --version >/dev/null 2>&1; then
    # `IDF_PATH` because the shim has to name the real script; a `PATH`-only export cannot be shimmed.
    if [ -n "${IDF_PYTHON_ENV_PATH:-}" ] && [ -n "${IDF_PATH:-}" ] \
        && [ -x "$IDF_PYTHON_ENV_PATH/bin/python" ]; then
        shim_dir="$ROOT/build/idf-bin"
        mkdir -p "$shim_dir"
        {
            printf '#!/bin/bash\n'
            printf '# Written by build/run-with-idf.sh: `idf.py` with the interpreter of its own venv.\n'
            printf 'exec "%s/bin/python" "%s/tools/idf.py" "$@"\n' "$IDF_PYTHON_ENV_PATH" "$IDF_PATH"
        } > "$shim_dir/idf.py"
        chmod +x "$shim_dir/idf.py"
        PATH="$shim_dir:$PATH"
        export PATH
        used="$used (+ an idf.py shim: its own script did not run)"
    fi
fi

# ── What it found, said plainly ──────────────────────────────────────────────────────────────────────
# A gate that cannot run must say where it looked — the rule this whole project keeps learning. Here it
# is the difference between "the build failed" and "your app has no ESP-IDF in its environment".
if [ -n "$used" ] && idf.py --version >/dev/null 2>&1; then
    say "ESP-IDF: $(idf.py --version 2>/dev/null | tail -1)"
    say "  from:  $used"
    say "  path:  ${IDF_PATH:-(not set — idf.py is on PATH without one)}"
    toolchain="$(command -v xtensa-esp32s3-elf-gcc || true)"
    [ -n "$toolchain" ] && say "  cc:    $toolchain"
else
    say "ESP-IDF: none found — launching anyway, and a chip build will fail inside the app."
    say "  looked at:"
    while IFS= read -r script; do
        [ -n "$script" ] && say "    $script"
    done <<EOF
$candidates
EOF
    say "    and a synthesised environment from IDF_PATH / IDF_TOOLS_PATH (neither usable)"
    say "  set SPIRE_IDF_EXPORT to your own exported environment, e.g."
    say "    SPIRE_IDF_EXPORT=~/esp/esp-idf/export.sh make run-idf"
fi

if [ -n "$CHECK_ONLY" ]; then
    [ -n "$used" ] && exit 0
    exit 1
fi

if [ -n "$RUN_AFTER" ]; then
    # `eval` because the command is a shell fragment a person typed (`idf.py build`, or a pipeline). It
    # runs in **this** environment, which is the whole point.
    eval "$RUN_AFTER"
    exit $?
fi

# ── Launch ───────────────────────────────────────────────────────────────────────────────────────────
# The **binary**, not `open`: `open` hands the app a fresh environment and would throw this one away. The
# bundle's executable is preferred so what runs is what `make app` assembled; the dev build is the
# fallback for someone who built the UI without assembling.
if [ -x "$APP_BIN" ]; then
    target="$APP_BIN"
elif [ -x "$DEV_BIN" ]; then
    target="$DEV_BIN"
else
    say "Nothing to launch: run 'make app' first (looked for $APP_BIN and $DEV_BIN)."
    exit 1
fi

say "launching $target"
exec "$target"
