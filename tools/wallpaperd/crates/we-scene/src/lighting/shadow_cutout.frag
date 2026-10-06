#version 300 es
precision highp float;
uniform sampler2D g_Texture0;
in vec2 uv;
void main() { if (texture(g_Texture0, uv).a < 0.5) discard; }
