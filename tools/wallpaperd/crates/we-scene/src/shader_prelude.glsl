#version 450
precision highp float;
precision highp int;
#define mul(a,b) ((b)*(a))
#define GLSL 1
#define float2 vec2
#define float3 vec3
#define float4 vec4
#define int2 ivec2
#define int3 ivec3
#define int4 ivec4
#define ddx dFdx
#define ddy(x) dFdy(-(x))
#define fmod wallpaperCompatFmod
#define frac fract
#define saturate(x) clamp(x,0.0,1.0)
#define lerp mix
#define CAST2 vec2
#define CAST3 vec3
#define CAST4 vec4
#define CAST3X3 mat3
#define CAST4X4 mat4
#define texSample2D texture
#define texSample2DLod textureLod
#define texSample2DGrad textureGrad
#define atan2 atan
float wallpaperCompatFmod(float x,float y) { return x-y*trunc(x/y); }
vec2 wallpaperCompatFmod(vec2 x,vec2 y) { return x-y*trunc(x/y); }
vec2 wallpaperCompatFmod(vec2 x,float y) { return x-y*trunc(x/y); }
vec2 wallpaperCompatFmod(float x,vec2 y) { return x-y*trunc(x/y); }
vec3 wallpaperCompatFmod(vec3 x,vec3 y) { return x-y*trunc(x/y); }
vec3 wallpaperCompatFmod(vec3 x,float y) { return x-y*trunc(x/y); }
vec3 wallpaperCompatFmod(float x,vec3 y) { return x-y*trunc(x/y); }
vec4 wallpaperCompatFmod(vec4 x,vec4 y) { return x-y*trunc(x/y); }
vec4 wallpaperCompatFmod(vec4 x,float y) { return x-y*trunc(x/y); }
vec4 wallpaperCompatFmod(float x,vec4 y) { return x-y*trunc(x/y); }
