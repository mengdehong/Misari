#version 300 es
precision highp float;
in vec3 a_Position;
in vec2 a_TexCoord;
uniform mat4 g_ModelViewProjectionMatrix;
out vec2 uv;
void main() {
    uv = a_TexCoord;
    gl_Position = g_ModelViewProjectionMatrix * vec4(a_Position, 1.0);
}
