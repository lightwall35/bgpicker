#version 440

layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;

layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 halfpixel;
    float offset;
    float saturation;
    float brightness;
};

layout(binding = 1) uniform sampler2D source;

void main() {
    vec2 uv = qt_TexCoord0;
    vec2 o = halfpixel * offset;
    vec4 sum = texture(source, uv + vec2(-o.x * 2.0, 0.0));
    sum += texture(source, uv + vec2(-o.x, o.y)) * 2.0;
    sum += texture(source, uv + vec2(0.0, o.y * 2.0));
    sum += texture(source, uv + vec2(o.x, o.y)) * 2.0;
    sum += texture(source, uv + vec2(o.x * 2.0, 0.0));
    sum += texture(source, uv + vec2(o.x, -o.y)) * 2.0;
    sum += texture(source, uv + vec2(0.0, -o.y * 2.0));
    sum += texture(source, uv + vec2(-o.x, -o.y)) * 2.0;
    vec4 col = sum / 12.0;
    if (saturation != 1.0 || brightness != 0.0) {
        float luma = dot(col.rgb, vec3(0.2126, 0.7152, 0.0722));
        col.rgb = mix(vec3(luma), col.rgb, saturation) + vec3(brightness);
        col.rgb = clamp(col.rgb, 0.0, 1.0);
    }
    fragColor = vec4(col.rgb, 1.0) * qt_Opacity;
}
