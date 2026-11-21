#version 450
// The exact scene in pixel coordinates, with row zero at the start of readback.
const vec2 vertices[3] = vec2[3](vec2(32.25, 32.25), vec2(224.25, 48.75), vec2(96.75, 220.5));
void main() {
    gl_Position = vec4(vertices[gl_VertexIndex] / 128.0 - 1.0, 0.0, 1.0);
}
