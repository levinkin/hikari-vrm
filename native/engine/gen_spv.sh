#!/bin/sh
cd "$(dirname "$0")"
{
for stage in surface_vertex:vert:SURFACE_VERTEX surface_fragment:frag:SURFACE_FRAGMENT screen_vertex:vert:SCREEN_VERTEX under_fragment:frag:UNDER_FRAGMENT share_fragment:frag:SHARE_FRAGMENT; do
    name=${stage%%:*}; rest=${stage#*:}; kind=${rest%%:*}; define=${rest#*:}
    echo "static const uint32_t ${name}_spv[] = {"
    glslc -O --target-env=vulkan1.3 -fshader-stage=$kind -D$define -mfmt=num -o - mtoon.glsl || exit 1
    echo "};"
done
} > mtoon_spv.inc
