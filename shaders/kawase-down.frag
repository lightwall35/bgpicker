#version 440

layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;

layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 halfpixel;
    float offset;
};

layout(binding = 1) uniform sampler2D source;

void main() {
    vec2 uv = qt_TexCoord0;
    vec2 o = halfpixel * offset;
    vec4 sum = texture(source, uv) * 4.0;
    sum += texture(source, uv - o);
    sum += texture(source, uv + o);
    sum += texture(source, uv + vec2(o.x, -o.y));
    sum += texture(source, uv - vec2(o.x, -o.y));
    fragColor = (sum / 8.0) * qt_Opacity;
}
