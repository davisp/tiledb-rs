#!/usr/bin/env python

# This is taken from the TileDB Release workflow found here:
#
# * https://github.com/TileDB-Inc/TileDB/blob/main/.github/workflows/release.yml
#
# The upstream version of this matrix does not include static builds which
# can be useful for projects like TileDB-Tables that want to distribute
# statically linked binaries.

import copy
import json

BASE_MATRIX = [
    {
        "platform": "linux-x86_64",
        "os": "ubuntu-20.04",
        "manylinux": "quay.io/pypa/manylinux_2_28_x86_64",
        "triplet": "x64-linux-release"
    },
    {
        "platform": "linux-x86_64-noavx2",
        "os": "ubuntu-20.04",
        "cmake_args": "-DCOMPILER_SUPPORTS_AVX2=OFF",
        "triplet": "x64-linux-release",
        "manylinux": "quay.io/pypa/manylinux_2_28_x86_64"
    },
    {
        "platform": "linux-aarch64",
        "os": "linux-arm64-ubuntu24",
        "triplet": "arm64-linux-release",
        "manylinux": "quay.io/pypa/manylinux_2_28_aarch64"
    },
    {
        "platform": "macos-x86_64",
        "os": "macos-13",
        "cmake_args": "-DCMAKE_OSX_ARCHITECTURES=x86_64",
        "MACOSX_DEPLOYMENT_TARGET": "11",
        "triplet": "x64-osx-release"
    },
    {
        "platform": "macos-arm64",
        "os": "macos-latest",
        "cmake_args": "-DCMAKE_OSX_ARCHITECTURES=arm64",
        "MACOSX_DEPLOYMENT_TARGET": "11",
        "triplet": "arm64-osx-release"
    }
]

BUILD_SHARED_LIBS = ["on", "off"]
VERSIONS = ["main", "2.27.0"]

def main():
    matrix = []
    for version in VERSIONS:
        for build_shared in BUILD_SHARED_LIBS:
            # Dynamically linked upstream releases are pulled from the
            # TileDB-Inc/TileDB repository releases so we skip re-building
            # them here.
            if version != "main" and build_shared == "ON":
                continue

            if build_shared == "ON":
                linkage = "Dynamic"
            else:
                linkage = "Static"

            for config in BASE_MATRIX:
                config = copy.deepcopy(config)
                config["version"] = version
                config["build_shared_libs"] = build_shared
                config["linkage"] = linkage
                matrix.append(config)

    print(json.dumps({"include": matrix}))


if __name__ == "__main__":
    main()
