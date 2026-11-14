// Shared UI/config contract. wallpaper_settings.py normalizes the same fields
// for shell callers; renderer commits receive its resolved immutable snapshot.
var choices = [
    {value: "fade", label: "淡化"},
    {value: "wipe", label: "斜扫"},
    {value: "wave", label: "波浪"},
    {value: "grow", label: "圆形展开"},
    {value: "outer", label: "圆形收拢"},
    {value: "random", label: "随机"},
    {value: "none", label: "直接"}
];

function settings(config) {
    var p = config && config.transition ? config.transition : {};
    function number(value, fallback, low, high) {
        var n = Number(value);
        return value === undefined || value === null || !isFinite(n)
            ? fallback : Math.max(low, Math.min(high, n));
    }
    function pair(value, fallback, low, high) {
        return Array.isArray(value) && value.length === 2
            ? [number(value[0], fallback[0], low, high), number(value[1], fallback[1], low, high)]
            : fallback;
    }
    var type = choices.some(function(c) { return c.value === p.type; }) ? p.type : "fade";
    var legacyDuration = config && config.parallax ? config.parallax.transitionMs : undefined;
    return {
        type: type,
        durationMs: Math.round(number(p.durationMs, number(legacyDuration, 800, 0, 5000), 0, 5000)),
        fps: Math.round(number(p.fps, 60, 1, 240)),
        angle: number(p.angle, 45, 0, 360),
        position: pair(p.position, [0.5, 0.5], 0, 1),
        wave: pair(p.wave, [20, 20], 1, 500)
    };
}

function kind(type) {
    return {fade: 0, wipe: 1, wave: 2, grow: 3, outer: 4, none: 5}[type] || 0;
}
