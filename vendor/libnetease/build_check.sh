#!/usr/bin/env bash
# Syntax-check build for libnetease using cl.exe directly (fast, no ninja needed).
# NOTE: the authoritative full build is tests/run_tests.sh / build_ninja.sh (CMake+ctest);
# this script only compiles every translation unit with the SAME feature macros the
# real build uses (NE_HAVE_CURL/NE_HAVE_ZLIB), so curl-path bugs are caught too.
# Usage: bash build_check.sh
set -u
cd "$(dirname "$0")"

VS="C:\\Program Files\\Microsoft Visual Studio\\18\\Community"
MSVC="$VS\\VC\\Tools\\MSVC\\14.52.36615"
SDK="C:\\Program Files (x86)\\Windows Kits\\10"
VCPKG="C:\\vcpkg\\installed\\x64-windows"

export INCLUDE="$MSVC\\include;$SDK\\Include\\10.0.26100.0\\ucrt;$SDK\\Include\\10.0.26100.0\\um;$SDK\\Include\\10.0.26100.0\\shared;$VCPKG\\include"
export LIB="$MSVC\\lib\\x64;$SDK\\Lib\\10.0.26100.0\\ucrt\\x64;$SDK\\Lib\\10.0.26100.0\\um\\x64;$VCPKG\\lib"
CL="/c/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.52.36615/bin/Hostx64/x64/cl.exe"
CFLAGS="/nologo /c /W4 /utf-8 /D_CRT_SECURE_NO_WARNINGS /DNE_HAVE_CURL /DNE_HAVE_ZLIB /Iinclude"

mkdir -p build/obj

SRCS=$(find src -name "*.c" | sort)
FAIL=0
for f in $SRCS; do
  obj="build/obj/$(echo "$f" | tr '/' '_' | sed 's/\.c$/.obj/')"
  if ! "$CL" $CFLAGS "/Fo$obj" "$f" 2>build/obj/err_$(basename "$f").log; then
    echo "FAIL(compile): $f"
    head -8 "build/obj/err_$(basename "$f").log"
    FAIL=1
  fi
done

if [ "$FAIL" = "1" ]; then
  echo "=== COMPILE ERRORS PRESENT ==="
  exit 1
fi
echo "=== ALL SOURCES COMPILED OK ==="
