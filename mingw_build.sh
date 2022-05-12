#!/bin/sh

set -e

 pacman --noconfirm --needed -S git mingw-w64-x86_64-cmake mingw-w64-x86_64-ninja mingw-w64-x86_64-gtest \
	mingw-w64-x86_64-giflib mingw-w64-x86_64-libpng mingw-w64-x86_64-libjpeg-turbo \
	mingw-w64-x86_64-cmake mingw-w64-x86_64-gtk4 mingw-w64-x86_64-libarchive mingw-w64-x86_64-libwebp \
	mingw-w64-x86_64-highway mingw-w64-x86_64-jemalloc bash


export HOME=/c/Users/$USER

#git clone https://github.com/libjxl/libjxl.git --recursive --shallow-submodules
#
#cd libjxl
#
#git checkout tags/v0.6.1 -f --recurse-submodules
#
#mkdir build && cd build
#
#cmake -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTING=OFF ..
#
#cmake --build . -- -j$(nproc)
#
#cmake --install . --prefix /mingw64
#
#cd ../..

curl https://sh.rustup.rs -sSf | sh -s -- --default-toolchain stable-x86_64-pc-windows-gnu -y
$HOME/.cargo/bin/rustup target add x86_64-pc-windows-gnu
# $HOME/.cargo/bin/rustup toolchain install stable-x86_64-pc-windows-gnu
# $HOME/.cargo/bin/rustup set default-host x86_64-pc-windows-gnu

export RUSTFLAGS="-C link-args=-Wl,-Bstatic -L/mingw64/lib" 

$HOME/.cargo/bin/cargo build --target x86_64-pc-windows-gnu --verbose --release
