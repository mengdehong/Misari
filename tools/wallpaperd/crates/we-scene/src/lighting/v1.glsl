uniform vec4 wallpaperLightOrigin[60];
uniform vec4 wallpaperLightColor[60];
uniform vec4 wallpaperLightDirection[60];
uniform vec4 wallpaperLightExtra[60];
uniform int wallpaperLightCount;
uniform highp sampler2DArray wallpaperShadowDepth;
uniform highp sampler2D wallpaperShadowMatrices;
uniform int wallpaperShadowReceive;

float WallpaperShadow(int i, vec3 worldPos) {
    vec4 extra = wallpaperLightExtra[i];
    if (wallpaperShadowReceive == 0 || extra.w == 0.0) return 1.0;
    int page = int(extra.w) - 1;
    if (wallpaperLightDirection[i].w == 0.0 ||
        (wallpaperLightDirection[i].w == 1.0 && extra.z == 1.0)) {
        vec3 delta = worldPos - wallpaperLightOrigin[i].xyz;
        vec3 axes = abs(delta);
        if (axes.x >= axes.y && axes.x >= axes.z) page += delta.x >= 0.0 ? 0 : 1;
        else if (axes.y >= axes.z) page += delta.y >= 0.0 ? 2 : 3;
        else page += delta.z >= 0.0 ? 4 : 5;
    }
    mat4 matrix = mat4(texelFetch(wallpaperShadowMatrices, ivec2(0, page), 0),
        texelFetch(wallpaperShadowMatrices, ivec2(1, page), 0),
        texelFetch(wallpaperShadowMatrices, ivec2(2, page), 0),
        texelFetch(wallpaperShadowMatrices, ivec2(3, page), 0));
    vec4 clip = matrix * vec4(worldPos, 1.0);
    if (clip.w <= 0.0) return 1.0;
    vec3 p = clip.xyz / clip.w * 0.5 + 0.5;
    if (any(lessThan(p, vec3(0.0))) || any(greaterThan(p, vec3(1.0)))) return 1.0;
    // Receiver-plane depth gradients give a projection-scaled bias. Avoid a
    // constant perspective-depth bias that would erase nearby point shadows.
    float bias = max(0.0000002, 1.5 * max(abs(dFdx(p.z)), abs(dFdy(p.z))));
    vec2 texel = 1.0 / vec2(textureSize(wallpaperShadowDepth, 0).xy);
    float lit = 0.0;
    for (int y = -1; y <= 1; ++y) for (int x = -1; x <= 1; ++x) {
        float depth = texture(wallpaperShadowDepth, vec3(p.xy + vec2(x,y) * texel, float(page))).r;
        lit += step(p.z - bias, depth);
    }
    return lit / 9.0;
}

vec3 PerformLighting_V1(vec3 worldPos, vec3 albedo, vec3 normal, vec3 viewDir,
    vec3 specularTint, vec3 baseReflectance, float roughness, float metallic) {
    vec3 light = vec3(0.0);
    for (int i = 0; i < wallpaperLightCount; ++i) {
        vec4 color = wallpaperLightColor[i];
        if (dot(color.rgb, color.rgb) == 0.0) continue;
        vec4 origin = wallpaperLightOrigin[i];
        vec4 direction = wallpaperLightDirection[i];
        if (direction.w == 3.0) {
            float shadow = WallpaperShadow(i, worldPos);
            light += ComputePBRLightShadowInfinite(normal, -direction.xyz, viewDir,
                albedo, color.rgb, specularTint, baseReflectance, max(roughness, 0.001), metallic, shadow);
        } else {
            vec3 delta = direction.w == 2.0
                ? PointSegmentDelta(worldPos, origin.xyz, wallpaperLightExtra[i].xyz)
                : origin.xyz - worldPos;
            float distance = length(delta);
            if (distance < 0.0001) delta = normal * 0.0001;
            vec3 radiance = color.rgb;
            if (direction.w == 1.0) {
                vec2 cones = wallpaperLightExtra[i].xy;
                float angle = -dot(normalize(delta), direction.xyz);
                radiance *= cones.x == cones.y ? step(cones.x, angle) : smoothstep(cones.y, cones.x, angle);
            }
            light += ComputePBRLightShadow(normal, delta, viewDir, albedo,
                radiance, color.w, origin.w, specularTint, baseReflectance, max(roughness, 0.001), metallic,
                WallpaperShadow(i, worldPos));
        }
    }
    return light;
}
