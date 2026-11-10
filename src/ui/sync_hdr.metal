#include <metal_stdlib>
using namespace metal;

struct Vertex {
    float4 position [[position]];
    float2 uv;
};

vertex Vertex icon_vertex(uint index [[vertex_id]]) {
    float2 uv = float2((index << 1) & 2, index & 2);
    return {float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0), uv};
}

float distance_to_link(float2 point, float2 start, float2 end) {
    float2 direction = normalize(end - start);
    start += direction * 0.12;
    end -= direction * 0.12;
    float2 link = end - start;
    return length(point - start - link * clamp(dot(point - start, link) / dot(link, link), 0.0, 1.0));
}

fragment float4 icon_fragment(Vertex input [[stage_in]], constant float4 &tint [[buffer(0)]]) {
    // The 14-point mesh has a 3-point margin for its halo.
    float2 point = (input.uv * 20.0 - 3.0) / 14.0;
    float2 nodes[3] = {float2(0.5, 0.19), float2(0.18, 0.76), float2(0.82, 0.76)};
    float distance = 1.0;
    for (uint i = 0; i < 3; ++i) {
        distance = min(distance, abs(length(point - nodes[i]) - 0.12));
        distance = min(distance, distance_to_link(point, nodes[i], nodes[(i + 1) % 3]));
    }
    float core = 1.0 - smoothstep(0.042, 0.065, distance);
    float halo = 0.12 * (1.0 - smoothstep(0.055, 0.19, distance));
    float coverage = max(core, halo);
    return float4(tint.rgb * coverage, coverage);
}
