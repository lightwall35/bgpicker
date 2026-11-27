//! Pure-Rust wallpaper picker UI engine, bounded async thumbnail/hi-res loader,
//! HiDPI CJK/Latin text atlas, and single-pass OpenGL ES 3.0 / GL 3.3 renderer.
//!
//! Designed for zero-FBO single-pass rendering with full visual and behavioral parity
//! with the original `Carousel.qml`, `CarouselMedia.js`, `SkewSelector.qml`, and `BgPicker.qml`.

pub mod ipc;
pub mod render;
pub mod state;
pub mod text;
pub mod thumbs;
pub mod wayland;

pub use render::{PickerError, PickerRenderer};
pub use state::{
    CardLayout, CarouselMode, ControlRect, DaemonStateSnapshot, KeyCode, KeyInput, MediaEntry,
    ModeButtonLayout, ModeIcon, Modifiers, MouseButton, Page, PageGeometry, PickerActionOut,
    PickerConfig, PickerInput, PickerUi, SelectorLayout, TransitionButtonLayout, filter_entries,
    page_geometry, selected_index,
};
pub use text::{GlyphQuad, TextAtlas, TextLayout};
pub use thumbs::{GpuTexture, LruCache, ThumbnailCache, ThumbnailCacheConfig};
