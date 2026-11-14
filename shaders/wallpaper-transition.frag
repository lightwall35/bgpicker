#version 440

layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    float transitionProgress;
    int effectKind;
    vec2 resolution;
    vec4 oldRect;
    vec4 newRect;
    vec2 origin;
    vec2 direction;
    vec2 wave;
};
layout(binding = 1) uniform sampler2D oldSource;
layout(binding = 2) uniform sampler2D newSource;

void main() {
    vec2 pixel = qt_TexCoord0 * resolution;
    vec2 oldUV = (pixel - oldRect.xy) / max(oldRect.zw, vec2(1.0));
    vec2 newUV = (pixel - newRect.xy) / max(newRect.zw, vec2(1.0));
    vec4 before = texture(oldSource, oldUV);
    vec4 after = texture(newSource, newUV);
    float p = clamp(transitionProgress, 0.0, 1.0);
    float mask = p;

    if (effectKind == 1 || effectKind == 2) {
        float span = max(dot(abs(direction), resolution), 1.0);
        float coordinate = dot(pixel - resolution * 0.5, direction) / span + 0.5;
        if (effectKind == 2) {
            vec2 tangent = vec2(-direction.y, direction.x);
            coordinate += sin(dot(pixel, tangent) / max(wave.x, 1.0) * 6.2831853) * wave.y / span;
        }
        float edge = 1.5 / span;
        // Extend the sweep beyond both edges so a wavy border enters/exits cleanly.
        float extra = effectKind == 2 ? wave.y / span : 0.0;
        float threshold = mix(-extra - edge, 1.0 + extra + edge, p);
        mask = 1.0 - smoothstep(threshold - edge, threshold + edge, coordinate);
    } else if (effectKind == 3 || effectKind == 4) {
        vec2 center = origin * resolution;
        float radius = max(length(max(center, resolution - center)), 1.0);
        float distance = length(pixel - center) / radius;
        float edge = 1.5 / radius;
        if (effectKind == 3)
            mask = 1.0 - smoothstep(p - edge, p + edge, distance);
        else
            mask = smoothstep(1.0 - p - edge, 1.0 - p + edge, distance);
    }

    if (p <= 0.0) mask = 0.0;
    if (p >= 1.0) mask = 1.0;
    fragColor = mix(before, after, mask) * qt_Opacity;
}
