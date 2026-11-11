#!/usr/bin/env bash
# Windows 完整构建: CMake(Ninja) + cl.exe + vcpkg, 跑 ctest 全部测试
# 用法: bash build_ninja.sh
set -u
cd "$(dirname "$0")"

VS="C:\\Program Files\\Microsoft Visual Studio\\18\\Community"
MSVC="$VS\\VC\\Tools\\MSVC\\14.52.36615"
SDK="C:\\Program Files (x86)\\Windows Kits\\10"
VCPKG="C:\\vcpkg\\installed\\x64-windows"
PYENV="C:\\Users\\liuca\\.workbuddy\\binaries\\python\\envs\\default"

export INCLUDE="$MSVC\\include;$SDK\\Include\\10.0.26100.0\\ucrt;$SDK\\Include\\10.0.26100.0\\um;$SDK\\Include\\10.0.26100.0\\shared;$VCPKG\\include"
export LIB="$MSVC\\lib\\x64;$SDK\\Lib\\10.0.26100.0\\ucrt\\x64;$SDK\\Lib\\10.0.26100.0\\um\\x64;$VCPKG\\lib"
export PATH="/c/tools:/c/Program Files/CMake/bin:$VCPKG/bin:$SDK\\bin\\10.0.26100.0\\x64:$PYENV/Scripts:$PYENV:$PATH"
CL_BIN="C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.52.36615/bin/Hostx64/x64/cl.exe"

# 1. 生成加密向量 (ref_impl.py 需要 pycryptodome, 已装于隔离 venv)
"$PYENV/Scripts/python.exe" tests/ref_impl.py > tests/expected.h \
  || { echo "REF_IMPL_FAIL"; exit 1; }

rm -rf build-ninja
cmake -S . -B build-ninja -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_C_COMPILER="$CL_BIN" \
  -DCMAKE_TOOLCHAIN_FILE=C:/vcpkg/scripts/buildsystems/vcpkg.cmake \
  || { echo "CONFIGURE_FAIL"; exit 1; }

cmake --build build-ninja || { echo "BUILD_FAIL"; exit 1; }

ctest --test-dir build-ninja --output-on-failure || { echo "CTEST_FAIL"; exit 1; }
echo "ALL_PASS"
