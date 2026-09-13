# bgpicker-awww-swaybg

A fullscreen skewed carousel wallpaper picker that simultaneously switches both
**awww** (workspace wallpaper) and **swaybg** (background wallpaper) backends.

Built on top of [wallpaperCarousel](https://github.com/motor-dev/wallpaperCarousel)'s
`Carousel.qml` — runs as a standalone [Quickshell](https://quickshell.outfoxxed.me/)
daemon, no DankMaterialShell or Noctalia required.

## Features

- **100% visual fidelity** with wallpaperCarousel — parallelogram-skewed cards,
  distance-based scale/opacity falloff, elastic bounce animations, fullscreen overlay
- **Dual-backend switching** — awww (with fade transition) + swaybg (seamless, no
  black flash) are switched simultaneously when you pick a wallpaper
- **Keyboard navigation** — `←` / `→` to browse, `Enter` to apply, `Escape` to close
- **Search** — `Ctrl+F` to filter wallpapers by filename
- **Hold to preview** — dwell on a tile to expand it for a larger preview
- **Three scroll modes** — infinite loop, wrap, or standard

## Requirements

- [Quickshell](https://quickshell.outfoxxed.me/) ≥ 0.3.0
- [awww](https://github.com/anyrun-org/awww) (workspace wallpaper daemon)
- [swaybg](https://github.com/swaywm/swaybg) (background wallpaper)
- A Wayland compositor with wlr-layer-shell support (e.g. Niri, Hyprland, Sway)

## Setup

### 1. Wallpaper directories

Place your wallpapers in both directories with **matching filenames**:

```
~/swaybg_wallpapers/
├── A.png
├── B.png
└── C.png

~/awww_wallpapers/
├── A.png    ← same filename, can be a different image
├── B.png
└── C.png
```

When you select `A.png` in the carousel, `swaybg` gets `~/swaybg_wallpapers/A.png`
and `awww` gets `~/awww_wallpapers/A.png`.

### 2. Start the daemon

```bash
# Start the bgpicker daemon (stays in foreground)
qs -p /home/lightwall/playground/bgpicker-awww-swaybg

# Or use the convenience script (auto-daemonizes)
./bgpicker.sh start
```

Or add to your compositor startup (e.g. in `~/.config/niri/config.kdl`):

```kdl
spawn-at-startup "qs" "-p" "/home/lightwall/playground/bgpicker-awww-swaybg" "-n" "-d"
```

### 3. Bind keys

#### Niri (`~/.config/niri/config.kdl`)

```kdl
binds {
    // Toggle the wallpaper carousel
    Mod+W { spawn "qs" "ipc" "-p" "/home/lightwall/playground/bgpicker-awww-swaybg" "call" "wallpaperCarousel" "toggle"; }

    // Or use the convenience script
    // Mod+W { spawn "/home/lightwall/playground/bgpicker-awww-swaybg/bgpicker.sh" "toggle"; }

    // Cycle through wallpapers
    Mod+Shift+Right { spawn "qs" "ipc" "-p" "/home/lightwall/playground/bgpicker-awww-swaybg" "call" "wallpaperCarousel" "cycleNext"; }
    Mod+Shift+Left  { spawn "qs" "ipc" "-p" "/home/lightwall/playground/bgpicker-awww-swaybg" "call" "wallpaperCarousel" "cyclePrevious"; }
}
```

#### Hyprland (`~/.config/hypr/hyprland.conf`)

```ini
bind = SUPER, W, exec, qs ipc -p /home/lightwall/playground/bgpicker-awww-swaybg call wallpaperCarousel toggle
bind = SUPER SHIFT, Right, exec, qs ipc -p /home/lightwall/playground/bgpicker-awww-swaybg call wallpaperCarousel cycleNext
bind = SUPER SHIFT, Left, exec, qs ipc -p /home/lightwall/playground/bgpicker-awww-swaybg call wallpaperCarousel cyclePrevious
```

## IPC Commands

| Command | Description |
|---------|-------------|
| `qs ipc -p <path> call wallpaperCarousel toggle` | Toggle the overlay |
| `qs ipc -p <path> call wallpaperCarousel open` | Open the overlay |
| `qs ipc -p <path> call wallpaperCarousel close` | Close the overlay |
| `qs ipc -p <path> call wallpaperCarousel cycleNext` | Open + highlight next |
| `qs ipc -p <path> call wallpaperCarousel cyclePrevious` | Open + highlight previous |

Where `<path>` is `/home/lightwall/playground/bgpicker-awww-swaybg`.

## Keyboard shortcuts (when overlay is open)

| Key | Action |
|-----|--------|
| `←` / `H` | Previous wallpaper |
| `→` / `L` | Next wallpaper |
| `Enter` | Apply selected wallpaper |
| `Escape` | Close overlay |
| `Ctrl+F` | Open search bar |
| `Home` | Jump to first |
| `End` | Jump to last |

## Configuration

Edit the `cfg` object in `BgPicker.qml` to customize the carousel appearance:

| Setting | Default | Description |
|---------|---------|-------------|
| `carouselMode` | `"wrap"` | `"standard"`, `"wrap"`, or `"infinite"` |
| `overlayOpacity` | `80` | Background dimming (0–100) |
| `cornerRadius` | `12` | Card corner rounding in pixels |
| `itemWidth` | `300` | Card width in pixels |
| `itemHeight` | `420` | Card height in pixels |
| `borderWidth` | `3` | Card border width |
| `selectedScale` | `108` | Center card scale (percentage) |
| `expandSelected` | `"false"` | Widen the center card |
| `enableHoldExpand` | `"true"` | Hold-to-preview mode |
| `holdDelay` | `1500` | Hold delay in ms before expanding |

To change wallpaper directories, edit the `dirSwaybg` and `dirAwww` properties
at the top of `BgPicker.qml`.

## File Structure

```
bgpicker-awww-swaybg/
├── shell.qml              # Quickshell entry point
├── BgPicker.qml           # Wrapper: connects Carousel to awww/swaybg
├── Carousel.qml           # Core carousel UI (from wallpaperCarousel, unmodified)
├── switch_wallpaper.sh    # Helper: performs the actual awww + swaybg switch
├── bgpicker.sh            # Convenience script: daemon lifecycle + IPC
├── manifest.conf          # Quickshell config metadata
└── README.md              # This file
```

## Credits

- Carousel UI: [wallpaperCarousel](https://github.com/motor-dev/wallpaperCarousel) by yngwe
- Original wallpaper picker concept: [ilyamiro](https://github.com/ilyamiro/nixos-configuration)
