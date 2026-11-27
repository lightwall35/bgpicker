//! Picker UI state machine, animations, layout geometry, and exact skewed hit testing.
//!
//! Mirrors `Carousel.qml`, `CarouselMedia.js`, `SkewSelector.qml`, and `BgPicker.qml`.

use bgpicker_core::{
    CatalogEntry, EngineKind, TRANSITION_CHOICES, TransitionType, VideoMode, WallpaperMode,
};
use bgpicker_gfx::{ease_in_out_cubic, ease_out_cubic};
use percent_encoding::percent_decode_str;

/// Active media catalog page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Page {
    #[default]
    Static,
    Video,
}

impl Page {
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Video => "video",
        }
    }

    #[inline]
    pub const fn index(self) -> usize {
        match self {
            Self::Static => 0,
            Self::Video => 1,
        }
    }

    #[inline]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Static => Self::Video,
            Self::Video => Self::Static,
        }
    }
}

/// Carousel index wrapping behavior (`Carousel.qml::_carouselMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CarouselMode {
    #[default]
    Wrap,
    Standard,
}

/// Visual & behavioral configuration matching `BgPicker.qml` / `Carousel.qml`.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerConfig {
    pub carousel_mode: CarouselMode,
    pub overlay_opacity: f32,
    pub corner_radius: f32,
    pub item_width: f32,
    pub item_height: f32,
    pub border_width: f32,
    pub skew_factor: f32,
    pub selected_scale: f32,
    pub base_scale: f32,
    pub expand_selected: bool,
    pub expand_multiplier: f32,
    pub enable_hold_expand: bool,
    pub hold_expand_ratio: f32,
    pub hold_delay_s: f32,
    pub page_travel: f32,
    pub wheel_threshold: f32,
    pub wallpaper_folder: String,
    pub video_folder: String,
    pub has_wallpaper_configured: bool,
    pub shell_settings_hint: String,
}

impl Default for PickerConfig {
    fn default() -> Self {
        Self {
            carousel_mode: CarouselMode::Wrap,
            overlay_opacity: 0.80,
            corner_radius: 12.0,
            item_width: 300.0,
            item_height: 420.0,
            border_width: 0.0,
            skew_factor: -0.35,
            selected_scale: 1.08,
            base_scale: 0.75,
            expand_selected: false,
            expand_multiplier: 1.20,
            enable_hold_expand: true,
            hold_expand_ratio: 0.35,
            hold_delay_s: 1.5,
            page_travel: 180.0,
            wheel_threshold: 40.0,
            wallpaper_folder: String::new(),
            video_folder: String::new(),
            has_wallpaper_configured: true,
            shell_settings_hint: "Open your shell settings -> Wallpaper to configure a wallpaper directory.".to_string(),
        }
    }
}

/// Normalized catalog item for display and decoding (`CarouselMedia.js::videos`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaEntry {
    pub file_name: String,
    pub thumbnail_path: String,
    pub original_path: String,
}

impl MediaEntry {
    /// Converts a `bgpicker-core` [`CatalogEntry`] to a [`MediaEntry`].
    pub fn from_catalog(entry: &CatalogEntry) -> Self {
        let original = if !entry.original_path.is_empty() {
            entry.original_path.clone()
        } else {
            decode_file_url(&entry.file_url)
        };
        let thumb_source = if !entry.thumbnail_url.is_empty() {
            decode_file_url(&entry.thumbnail_url)
        } else if !entry.file_url.is_empty() {
            decode_file_url(&entry.file_url)
        } else {
            original.clone()
        };
        Self {
            file_name: entry.file_name.clone(),
            thumbnail_path: thumb_source,
            original_path: original,
        }
    }
}

fn decode_file_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("file://") {
        percent_decode_str(rest).decode_utf8_lossy().into_owned()
    } else {
        url.to_string()
    }
}

/// Filters `entries` by case-insensitive substring match (`CarouselMedia.js::filter`).
pub fn filter_entries(entries: &[MediaEntry], query: &str) -> Vec<MediaEntry> {
    if query.is_empty() {
        return entries.to_vec();
    }
    let needle = query.to_lowercase();
    entries
        .iter()
        .filter(|e| e.file_name.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// Selects the initial entry index (`CarouselMedia.js::selectedIndex`).
///
/// Prefers `preferred_name` if non-empty, otherwise matches the basename of `current_path`.
/// Returns `Some(0)` if `entries` is non-empty and no match is found, or `None` if empty.
pub fn selected_index(
    entries: &[MediaEntry],
    preferred_name: Option<&str>,
    current_path: &str,
) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    let target_name = preferred_name
        .filter(|s| !s.is_empty())
        .or_else(|| current_path.rsplit('/').next().filter(|s| !s.is_empty()));
    if let Some(name) = target_name
        && let Some(pos) = entries.iter().position(|e| e.file_name == name)
    {
        return Some(pos);
    }
    Some(0)
}

/// Outgoing/incoming page cross-fade & slide geometry (`CarouselMedia.js::pageGeometry`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeometry {
    pub outgoing_y: f32,
    pub incoming_y: f32,
    pub outgoing_opacity: f32,
    pub incoming_opacity: f32,
}

/// Computes outgoing and incoming Y offsets and opacities for a vertical page switch.
pub fn page_geometry(progress: f32, direction: f32, travel: f32) -> PageGeometry {
    let t = progress.clamp(0.0, 1.0);
    PageGeometry {
        outgoing_y: direction * travel * t,
        incoming_y: -direction * travel * (1.0 - t),
        outgoing_opacity: 1.0 - t,
        incoming_opacity: t,
    }
}

/// Snapshot of daemon state pushed into [`PickerUi`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DaemonStateSnapshot {
    pub engine: EngineKind,
    pub wallpaper_mode: WallpaperMode,
    pub video_mode: VideoMode,
    pub transition_type: TransitionType,
    pub current_wallpaper: String,
    pub current_video: String,
    pub busy: bool,
    pub error: String,
}

/// Keyboard modifier state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Logical key codes handled by [`PickerUi`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Escape,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Tab,
    Char(char),
}

/// Keyboard press event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    pub key: KeyCode,
    pub modifiers: Modifiers,
}

/// Mouse button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Input events accepted by [`PickerUi::handle_input`].
#[derive(Debug, Clone, PartialEq)]
pub enum PickerInput {
    KeyInput(KeyInput),
    TextInput(String),
    PointerMove { x: f32, y: f32 },
    PointerLeave,
    PointerPress { x: f32, y: f32, button: MouseButton },
    PointerRelease { x: f32, y: f32, button: MouseButton },
    PointerClick { x: f32, y: f32, button: MouseButton },
    /// Vertical/horizontal scroll delta.
    ///
    /// Uses Qt `angleDelta.y` sign convention matching `Carousel.qml`:
    /// `dy < 0.0` scrolls down toward [`Page::Video`], `dy > 0.0` scrolls up toward [`Page::Static`].
    PointerWheel { dx: f32, dy: f32 },
    SetCatalog {
        images: Vec<CatalogEntry>,
        videos: Vec<CatalogEntry>,
    },
    SetDaemonState(DaemonStateSnapshot),
    SetViewport {
        width: f32,
        height: f32,
        scale: f32,
    },
    StartOpen,
    StartClose,
    CycleNext,
    CyclePrevious,
    ShowPage(Page),
    ApplySelection,
}

/// Side-effect actions emitted by [`PickerUi`] for the shell/IPC layer to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerActionOut {
    ApplyStatic(String),
    ApplyVideo(VideoMode, String),
    ToggleMode,
    SelectTransition(TransitionType),
    RequestCatalog,
    Exit,
}

/// Vector icon kind for mode selector buttons (`SkewSelector.qml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeIcon {
    Frame,
    Layers,
}

/// Axis-aligned logical rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ControlRect {
    #[inline]
    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.width && py >= self.y && py <= self.y + self.height
    }
}

/// Resolved per-card visual state for rendering and hit testing.
#[derive(Debug, Clone, PartialEq)]
pub struct CardLayout {
    pub index: usize,
    pub file_name: String,
    pub thumbnail_path: String,
    pub original_path: String,
    pub center_x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub opacity: f32,
    pub hi_res_opacity: f32,
    pub visual_x_offset: f32,
    pub dist_from_center: usize,
    pub z_order: i32,
    pub is_current: bool,
    pub is_held: bool,
    pub is_confirmed: bool,
    pub is_hovered: bool,
}

/// Resolved mode selector button visual state.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeButtonLayout {
    pub index: usize,
    pub label: &'static str,
    pub icon: ModeIcon,
    pub center_x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub bg_alpha: f32,
    pub content_opacity: f32,
    pub selected: bool,
    pub hovered: bool,
}

/// Resolved transition selector button visual state.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionButtonLayout {
    pub index: usize,
    pub transition: TransitionType,
    pub label: &'static str,
    pub center_x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub bg_alpha: f32,
    pub content_opacity: f32,
    pub selected: bool,
    pub hovered: bool,
}

/// Complete resolved layout of bottom `SkewSelector` controls.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectorLayout {
    pub stacked: bool,
    pub mode_available: bool,
    pub mode_opacity: f32,
    pub mode_pill_center_x: f32,
    pub mode_pill_center_y: f32,
    pub mode_pill_width: f32,
    pub mode_pill_height: f32,
    pub mode_buttons: [ModeButtonLayout; 2],
    pub transition_pill_center_x: f32,
    pub transition_pill_center_y: f32,
    pub transition_pill_width: f32,
    pub transition_pill_height: f32,
    pub transition_buttons: Vec<TransitionButtonLayout>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EasingKind {
    Linear,
    OutCubic,
    InOutCubic,
    OutBack,
    InOutQuad,
}

#[inline]
fn eval_easing(kind: EasingKind, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match kind {
        EasingKind::Linear => t,
        EasingKind::OutCubic => ease_out_cubic(t),
        EasingKind::InOutCubic => ease_in_out_cubic(t),
        EasingKind::OutBack => {
            const S: f32 = 1.701_58;
            let u = t - 1.0;
            1.0 + (S + 1.0) * u * u * u + S * u * u
        }
        EasingKind::InOutQuad => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                let u = -2.0 * t + 2.0;
                1.0 - 0.5 * u * u
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AnimFloat {
    current: f32,
    from: f32,
    target: f32,
    elapsed: f32,
    duration: f32,
    easing: EasingKind,
}

impl AnimFloat {
    fn new(initial: f32) -> Self {
        Self {
            current: initial,
            from: initial,
            target: initial,
            elapsed: 0.0,
            duration: 0.0,
            easing: EasingKind::Linear,
        }
    }

    fn snap(&mut self, value: f32) {
        self.current = value;
        self.from = value;
        self.target = value;
        self.elapsed = 0.0;
        self.duration = 0.0;
    }

    fn set_target(&mut self, target: f32, duration: f32, easing: EasingKind) {
        if (self.target - target).abs() <= 1e-4 {
            return;
        }
        if duration <= 0.0 {
            self.snap(target);
            return;
        }
        self.from = self.current;
        self.target = target;
        self.elapsed = 0.0;
        self.duration = duration;
        self.easing = easing;
    }

    fn is_animating(&self) -> bool {
        self.duration > 0.0 && self.elapsed < self.duration && (self.current - self.target).abs() > 1e-4
    }

    fn tick(&mut self, dt_s: f32) -> bool {
        if self.duration <= 0.0 || self.elapsed >= self.duration {
            self.current = self.target;
            return false;
        }
        self.elapsed = (self.elapsed + dt_s.max(0.0)).min(self.duration);
        let t = (self.elapsed / self.duration).clamp(0.0, 1.0);
        let eased = eval_easing(self.easing, t);
        self.current = self.from + (self.target - self.from) * eased;
        if self.elapsed >= self.duration {
            self.current = self.target;
            self.duration = 0.0;
        }
        true
    }
}

/// Critically-damped analytical spring for continuous, jump-free retargeting
/// (C1-continuous position and velocity across hover/scroll/drag transitions).
#[derive(Debug, Clone, Copy, PartialEq)]
struct SmoothSpring {
    current: f32,
    velocity: f32,
    target: f32,
    omega: f32,
}

impl SmoothSpring {
    fn new(initial: f32, omega: f32) -> Self {
        Self {
            current: initial,
            velocity: 0.0,
            target: initial,
            omega,
        }
    }

    fn snap(&mut self, value: f32) {
        self.current = value;
        self.velocity = 0.0;
        self.target = value;
    }

    fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    fn is_animating(&self) -> bool {
        (self.current - self.target).abs() > 1e-3 || self.velocity.abs() > 1e-2
    }

    fn tick(&mut self, dt_s: f32) -> bool {
        if !self.is_animating() {
            self.current = self.target;
            self.velocity = 0.0;
            return false;
        }
        let dt = dt_s.max(0.0);
        if dt <= 0.0 {
            return false;
        }
        let x0 = self.current - self.target;
        let v0 = self.velocity;
        let w = self.omega;
        let e = (-w * dt).exp();
        let c2 = v0 + w * x0;
        self.current = self.target + e * (x0 + c2 * dt);
        self.velocity = e * (v0 - w * c2 * dt);
        if !self.is_animating() {
            self.current = self.target;
            self.velocity = 0.0;
        }
        true
    }
}

#[derive(Debug, Clone)]
struct CardAnimState {
    width: AnimFloat,
    height: AnimFloat,
    visual_x_offset: AnimFloat,
    scale: SmoothSpring,
    opacity: SmoothSpring,
    hi_res_opacity: AnimFloat,
}

#[derive(Debug, Clone)]
struct ButtonAnimState {
    scale: AnimFloat,
    bg_alpha: AnimFloat,
    content_opacity: AnimFloat,
}

impl ButtonAnimState {
    fn new(selected: bool) -> Self {
        Self {
            scale: AnimFloat::new(1.0),
            bg_alpha: AnimFloat::new(0x05 as f32 / 255.0),
            content_opacity: AnimFloat::new(if selected { 1.0 } else { 0.46 }),
        }
    }

    fn update_targets(&mut self, selected: bool, hovered: bool) {
        let target_scale = if hovered && !selected { 1.035 } else { 1.0 };
        let target_bg = if hovered && !selected {
            0x12 as f32 / 255.0
        } else {
            0x05 as f32 / 255.0
        };
        let target_content = if selected {
            1.0
        } else if hovered {
            0.8
        } else {
            0.46
        };
        self.scale.set_target(target_scale, 0.25, EasingKind::OutBack);
        self.bg_alpha.set_target(target_bg, 0.16, EasingKind::Linear);
        self.content_opacity
            .set_target(target_content, 0.18, EasingKind::Linear);
    }

    fn tick(&mut self, dt_s: f32) -> bool {
        let a = self.scale.tick(dt_s);
        let b = self.bg_alpha.tick(dt_s);
        let c = self.content_opacity.tick(dt_s);
        a || b || c
    }

    fn is_animating(&self) -> bool {
        self.scale.is_animating() || self.bg_alpha.is_animating() || self.content_opacity.is_animating()
    }
}

/// Checks whether `(px, py)` lies inside a centered skewed rectangle `(cx, cy, w, h, skew)`.
#[inline]
pub fn point_in_skewed_rect(
    px: f32,
    py: f32,
    cx: f32,
    cy: f32,
    width: f32,
    height: f32,
    skew: f32,
) -> bool {
    let v = py - cy;
    let u = (px - cx) - skew * v;
    u.abs() <= width * 0.5 && v.abs() <= height * 0.5
}

const SCROLL_DURATION_S: f32 = 0.25;
const HOLD_EXPAND_DURATION_S: f32 = 0.40;
const CARD_SCALE_SPRING_OMEGA: f32 = 20.0;
const CARD_OPACITY_SPRING_OMEGA: f32 = 20.0;
const HIRES_FADE_DURATION_S: f32 = 0.30;
const CONFIRM_DURATION_S: f32 = 0.30;
const PAGE_SWITCH_DURATION_S: f32 = 0.30;
const PAGE_COOLDOWN_S: f32 = 0.18;
const PANEL_OPEN_DURATION_S: f32 = 0.24;
const PANEL_CLOSE_DURATION_S: f32 = 0.18;
const SEARCH_OPEN_DURATION_S: f32 = 0.22;
const SEARCH_CLOSE_DURATION_S: f32 = 0.16;
const SEARCH_PANEL_CLOSE_DURATION_S: f32 = 0.14;
const PILL_SLIDE_DURATION_S: f32 = 0.30;
const CURSOR_BLINK_HALF_PERIOD_S: f32 = 0.53;
const DRAG_THRESHOLD_PX: f32 = 6.0;
const FLICK_MOMENTUM_S: f32 = 0.14;
const FLICK_MAX_CARDS: f32 = 4.0;
const DRAG_RUBBER_BAND_FACTOR: f32 = 0.28;
const DRAG_RUBBER_BAND_MAX_CARDS: f32 = 0.45;

/// Complete pure-Rust picker UI state machine.
#[derive(Debug, Clone)]
pub struct PickerUi {
    config: PickerConfig,
    viewport_w: f32,
    viewport_h: f32,
    scale: f32,

    visible: bool,
    closing: bool,
    page: Page,
    images: Vec<MediaEntry>,
    videos: Vec<MediaEntry>,
    active_entries: Vec<MediaEntry>,
    current_index: usize,
    remembered_selection: [Option<String>; 2],

    daemon: DaemonStateSnapshot,

    search_visible: bool,
    search_text: String,
    search_display_text: String,
    search_progress: AnimFloat,
    cursor_blink_s: f32,
    monotonic_s: f32,

    pointer_x: f32,
    pointer_y: f32,
    pointer_inside: bool,
    hovered_card: Option<usize>,
    hovered_mode_button: Option<usize>,
    hovered_transition_button: Option<usize>,

    // Mouse horizontal drag & flick state
    left_button_down: bool,
    press_x: f32,
    press_y: f32,
    press_hit_control: bool,
    drag_eligible: bool,
    drag_started: bool,
    drag_anchor_x: f32,
    drag_start_scroll: f32,
    drag_start_index: usize,
    drag_last_x: f32,
    drag_last_sample_s: f32,
    drag_velocity_px_s: f32,

    panel_progress: AnimFloat,
    scroll_index: AnimFloat,
    card_anims: Vec<CardAnimState>,

    // Hold expand preview state
    hold_armed: bool,
    hold_elapsed_s: f32,
    held_index: Option<usize>,
    hi_res_ready_path: Option<String>,

    // Confirm animation state
    confirming_index: Option<usize>,
    pending_pick_path: Option<String>,
    confirm_elapsed_s: f32,

    // Vertical page switch state
    wheel_accum: f32,
    page_animating: bool,
    page_progress: AnimFloat,
    page_direction: f32,
    page_cooldown_s: f32,
    outgoing_cards: Vec<CardLayout>,

    // Bottom segmented control animations
    mode_pill_index: AnimFloat,
    mode_buttons: [ButtonAnimState; 2],
    transition_pill_index: AnimFloat,
    transition_buttons: Vec<ButtonAnimState>,

    dirty: bool,
}

impl PickerUi {
    /// Creates a new [`PickerUi`] with the given configuration and initial logical viewport size.
    pub fn new(config: PickerConfig, viewport_w: f32, viewport_h: f32) -> Self {
        let transition_buttons = (0..TRANSITION_CHOICES.len())
            .map(|i| ButtonAnimState::new(i == 0))
            .collect();
        Self {
            config,
            viewport_w: viewport_w.max(320.0),
            viewport_h: viewport_h.max(240.0),
            scale: 1.0,
            visible: false,
            closing: false,
            page: Page::Static,
            images: Vec::new(),
            videos: Vec::new(),
            active_entries: Vec::new(),
            current_index: 0,
            remembered_selection: [None, None],
            daemon: DaemonStateSnapshot::default(),
            search_visible: false,
            search_text: String::new(),
            search_display_text: String::new(),
            search_progress: AnimFloat::new(0.0),
            cursor_blink_s: 0.0,
            monotonic_s: 0.0,
            pointer_x: -1.0,
            pointer_y: -1.0,
            pointer_inside: false,
            hovered_card: None,
            hovered_mode_button: None,
            hovered_transition_button: None,
            left_button_down: false,
            press_x: 0.0,
            press_y: 0.0,
            press_hit_control: false,
            drag_eligible: false,
            drag_started: false,
            drag_anchor_x: 0.0,
            drag_start_scroll: 0.0,
            drag_start_index: 0,
            drag_last_x: 0.0,
            drag_last_sample_s: 0.0,
            drag_velocity_px_s: 0.0,
            panel_progress: AnimFloat::new(0.0),
            scroll_index: AnimFloat::new(0.0),
            card_anims: Vec::new(),
            hold_armed: false,
            hold_elapsed_s: 0.0,
            held_index: None,
            hi_res_ready_path: None,
            confirming_index: None,
            pending_pick_path: None,
            confirm_elapsed_s: 0.0,
            wheel_accum: 0.0,
            page_animating: false,
            page_progress: AnimFloat::new(1.0),
            page_direction: -1.0,
            page_cooldown_s: 0.0,
            outgoing_cards: Vec::new(),
            mode_pill_index: AnimFloat::new(0.0),
            mode_buttons: [ButtonAnimState::new(true), ButtonAnimState::new(false)],
            transition_pill_index: AnimFloat::new(0.0),
            transition_buttons,
            dirty: true,
        }
    }

    #[inline]
    pub fn config(&self) -> &PickerConfig {
        &self.config
    }

    #[inline]
    pub fn config_mut(&mut self) -> &mut PickerConfig {
        self.dirty = true;
        &mut self.config
    }

    #[inline]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    #[inline]
    pub fn is_closing(&self) -> bool {
        self.closing
    }

    /// Current eased panel open/close progress in `0.0..=1.0`.
    #[inline]
    pub fn panel_progress(&self) -> f32 {
        self.panel_progress.current.clamp(0.0, 1.0)
    }

    /// Smooth dissolve multiplier (`1.0 -> 0.0`) during confirm-pick animation.
    #[inline]
    pub fn confirm_fade(&self) -> f32 {
        if self.confirming_index.is_some() {
            1.0 - ease_out_cubic((self.confirm_elapsed_s / CONFIRM_DURATION_S).clamp(0.0, 1.0))
        } else {
            1.0
        }
    }

    /// Effective combined overlay fade (`panel_progress * confirm_fade`) used for
    /// the scrim, bottom selectors, error banner, empty text, and search bar.
    #[inline]
    pub fn overlay_alpha(&self) -> f32 {
        (self.panel_progress() * self.confirm_fade()).clamp(0.0, 1.0)
    }

    #[inline]
    pub fn page(&self) -> Page {
        self.page
    }

    #[inline]
    pub fn viewport(&self) -> (f32, f32) {
        (self.viewport_w, self.viewport_h)
    }

    #[inline]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    #[inline]
    pub fn active_entries(&self) -> &[MediaEntry] {
        &self.active_entries
    }

    /// Total unfiltered count on the active page (`Carousel.qml::activeEntryCount`).
    #[inline]
    pub fn active_unfiltered_count(&self) -> usize {
        self.page_entries(self.page).len()
    }

    #[inline]
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    #[inline]
    pub fn current_entry(&self) -> Option<&MediaEntry> {
        self.active_entries.get(self.current_index)
    }

    #[inline]
    pub fn held_index(&self) -> Option<usize> {
        self.held_index
    }

    /// Returns the original static image path that should be decoded at high resolution
    /// while the user holds on a card (`Some(path)` only on `Page::Static` when held).
    pub fn active_hires_path(&self) -> Option<&str> {
        if !self.visible || self.closing || self.page != Page::Static {
            return None;
        }
        let idx = self.held_index?;
        let entry = self.active_entries.get(idx)?;
        if entry.original_path.is_empty() {
            None
        } else {
            Some(entry.original_path.as_str())
        }
    }

    /// Notifies the UI whether the high-res texture for `path` is now resident on GPU,
    /// triggering the `300ms` cross-fade over the thumbnail.
    pub fn set_hires_ready(&mut self, path: Option<&str>) {
        let next = path.map(ToOwned::to_owned);
        if self.hi_res_ready_path != next {
            self.hi_res_ready_path = next;
            self.sync_card_targets(false);
            self.dirty = true;
        }
    }

    #[inline]
    pub fn confirming_index(&self) -> Option<usize> {
        self.confirming_index
    }

    #[inline]
    pub fn search_visible(&self) -> bool {
        self.search_visible
    }

    #[inline]
    pub fn search_text(&self) -> &str {
        &self.search_text
    }

    /// Current eased search box open/close progress in `0.0..=1.0`.
    #[inline]
    pub fn search_progress(&self) -> f32 {
        self.search_progress.current.clamp(0.0, 1.0)
    }

    /// Text to render inside the search bar (preserves snapshot while fading closed
    /// on page switch or panel close so placeholder text never flashes).
    #[inline]
    pub fn search_render_text(&self) -> &str {
        if self.search_visible {
            &self.search_text
        } else {
            &self.search_display_text
        }
    }

    #[inline]
    pub fn cursor_visible(&self) -> bool {
        self.search_visible
            && !self.closing
            && self.confirming_index.is_none()
            && (self.cursor_blink_s % (2.0 * CURSOR_BLINK_HALF_PERIOD_S))
                < CURSOR_BLINK_HALF_PERIOD_S
    }

    #[inline]
    pub fn daemon_state(&self) -> &DaemonStateSnapshot {
        &self.daemon
    }

    #[inline]
    pub fn error_message(&self) -> &str {
        &self.daemon.error
    }

    #[inline]
    pub fn scrim_opacity(&self) -> f32 {
        self.config.overlay_opacity * self.overlay_alpha()
    }

    #[inline]
    pub fn is_page_animating(&self) -> bool {
        self.page_animating
    }

    #[inline]
    pub fn current_page_geometry(&self) -> PageGeometry {
        if self.page_animating {
            page_geometry(
                self.page_progress.current,
                self.page_direction,
                self.config.page_travel,
            )
        } else {
            PageGeometry {
                outgoing_y: 0.0,
                incoming_y: 0.0,
                outgoing_opacity: 0.0,
                incoming_opacity: 1.0,
            }
        }
    }

    #[inline]
    pub fn outgoing_cards(&self) -> &[CardLayout] {
        if self.page_animating {
            &self.outgoing_cards
        } else {
            &[]
        }
    }

    #[inline]
    pub fn hovered_card(&self) -> Option<usize> {
        self.hovered_card
    }

    #[inline]
    pub fn hovered_mode_button(&self) -> Option<usize> {
        self.hovered_mode_button
    }

    #[inline]
    pub fn hovered_transition_button(&self) -> Option<usize> {
        self.hovered_transition_button
    }

    #[inline]
    pub fn is_dragging(&self) -> bool {
        self.drag_started
    }

    #[inline]
    pub fn scroll_pos(&self) -> f32 {
        self.scroll_index.current
    }

    /// Returns `true` if visual state is actively animating and requires continuous frame redraws.
    pub fn is_animating(&self) -> bool {
        if self.panel_progress.is_animating()
            || self.search_progress.is_animating()
            || self.closing
            || self.scroll_index.is_animating()
            || self.page_animating
            || self.confirming_index.is_some()
            || self.mode_pill_index.is_animating()
            || self.transition_pill_index.is_animating()
        {
            return true;
        }
        if self.mode_buttons.iter().any(ButtonAnimState::is_animating)
            || self
                .transition_buttons
                .iter()
                .any(ButtonAnimState::is_animating)
        {
            return true;
        }
        self.card_anims.iter().any(|c| {
            c.width.is_animating()
                || c.height.is_animating()
                || c.visual_x_offset.is_animating()
                || c.scale.is_animating()
                || c.opacity.is_animating()
                || c.hi_res_opacity.is_animating()
        })
    }

    /// Returns `true` if a new frame should be rendered now.
    pub fn needs_redraw(&self) -> bool {
        self.dirty || self.is_animating()
    }

    /// Marks the current visual state as rendered.
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Returns the shortest delay (in seconds) until the next timed state transition
    /// (such as the 1.5s hold-expand trigger, page cooldown, or search cursor blink)
    /// so the event loop can sleep with 0% CPU/GPU usage when idle.
    pub fn next_timer_delay_s(&self) -> Option<f32> {
        if !self.visible && !self.closing {
            return None;
        }
        if self.is_animating() {
            return Some(0.0);
        }
        let mut min_delay: Option<f32> = None;
        let mut consider = |d: f32| {
            let d = d.max(0.001);
            min_delay = Some(match min_delay {
                Some(prev) => prev.min(d),
                None => d,
            });
        };
        if self.hold_armed
            && !self.drag_started
            && self.held_index.is_none()
            && self.confirming_index.is_none()
            && !self.page_animating
            && !self.active_entries.is_empty()
        {
            consider((self.config.hold_delay_s - self.hold_elapsed_s).max(0.0));
        }
        if self.page_cooldown_s > 0.0 {
            consider(self.page_cooldown_s);
        }
        if self.search_visible {
            let phase = self.cursor_blink_s % CURSOR_BLINK_HALF_PERIOD_S;
            consider((CURSOR_BLINK_HALF_PERIOD_S - phase).max(0.01));
        }
        min_delay
    }

    /// Handles an input event and returns any actions to dispatch to the daemon/shell.
    pub fn handle_input(&mut self, input: PickerInput) -> Vec<PickerActionOut> {
        let mut actions = Vec::new();
        match input {
            PickerInput::SetViewport {
                width,
                height,
                scale,
            } => {
                let w = width.max(320.0);
                let h = height.max(240.0);
                let s = scale.clamp(0.5, 4.0);
                if (self.viewport_w - w).abs() > 0.5
                    || (self.viewport_h - h).abs() > 0.5
                    || (self.scale - s).abs() > 0.01
                {
                    self.viewport_w = w;
                    self.viewport_h = h;
                    self.scale = s;
                    self.sync_card_targets(true);
                    self.dirty = true;
                }
            }
            PickerInput::SetCatalog { images, videos } => {
                let had_entries = !self.active_entries.is_empty();
                let current_name = self
                    .current_entry()
                    .map(|e| e.file_name.clone())
                    .filter(|s| !s.is_empty());
                if let Some(name) = current_name {
                    self.remembered_selection[self.page.index()] = Some(name);
                }
                self.images = images.iter().map(MediaEntry::from_catalog).collect();
                self.videos = videos.iter().map(MediaEntry::from_catalog).collect();
                self.rebuild_active_entries(!had_entries);
                self.dirty = true;
            }
            PickerInput::SetDaemonState(snapshot) => {
                let prev_wallpaper = self.daemon.current_wallpaper.clone();
                let prev_video = self.daemon.current_video.clone();
                self.daemon = snapshot;
                self.sync_selector_targets(false);
                // If catalog was already loaded and no remembered selection exists, re-sync initial selection when current path arrives.
                if self.remembered_selection[self.page.index()].is_none()
                    && !self.active_entries.is_empty()
                    && self.held_index.is_none()
                    && ((self.page == Page::Static && prev_wallpaper != self.daemon.current_wallpaper)
                        || (self.page == Page::Video && prev_video != self.daemon.current_video))
                {
                    self.rebuild_active_entries(true);
                }
                self.dirty = true;
            }
            PickerInput::StartOpen => {
                self.start_open(&mut actions);
            }
            PickerInput::StartClose => {
                self.start_close(&mut actions);
            }
            PickerInput::CycleNext => {
                if !self.visible || self.closing {
                    self.start_open(&mut actions);
                } else {
                    self.step_selection(1);
                }
            }
            PickerInput::CyclePrevious => {
                if !self.visible || self.closing {
                    self.start_open(&mut actions);
                } else {
                    self.step_selection(-1);
                }
            }
            PickerInput::ShowPage(target) => {
                self.request_page_switch(target);
            }
            PickerInput::ApplySelection => {
                if self.visible && !self.closing {
                    self.confirm_pick(self.current_index);
                }
            }
            PickerInput::KeyInput(key) => {
                self.handle_key(key, &mut actions);
            }
            PickerInput::TextInput(text) => {
                if self.visible
                    && !self.closing
                    && self.search_visible
                    && self.confirming_index.is_none()
                {
                    let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !filtered.is_empty() {
                        self.search_text.push_str(&filtered);
                        self.cursor_blink_s = 0.0;
                        self.rebuild_active_entries(true);
                        self.dirty = true;
                    }
                }
            }
            PickerInput::PointerMove { x, y } => {
                self.handle_pointer_move(x, y);
            }
            PickerInput::PointerLeave => {
                self.handle_pointer_leave();
            }
            PickerInput::PointerPress { x, y, button } => {
                self.handle_pointer_press(x, y, button);
            }
            PickerInput::PointerRelease { x, y, button } => {
                self.handle_pointer_release(x, y, button, &mut actions);
            }
            PickerInput::PointerClick { x, y, button } => {
                self.pointer_x = x;
                self.pointer_y = y;
                self.pointer_inside = true;
                self.update_hover_state();
                if button == MouseButton::Left {
                    self.handle_left_click(x, y, &mut actions);
                }
            }
            PickerInput::PointerWheel { dx: _, dy } => {
                if !self.visible
                    || self.closing
                    || self.confirming_index.is_some()
                    || self.drag_started
                {
                    return actions;
                }
                if self.page_animating || self.page_cooldown_s > 0.0 {
                    self.wheel_accum = 0.0;
                } else if dy.abs() > 0.0 {
                    if dy * self.wheel_accum < 0.0 {
                        self.wheel_accum = 0.0;
                    }
                    self.wheel_accum += dy;
                    if self.wheel_accum.abs() >= self.config.wheel_threshold {
                        let target = if self.wheel_accum < 0.0 {
                            Page::Video
                        } else {
                            Page::Static
                        };
                        self.wheel_accum = 0.0;
                        self.request_page_switch(target);
                    }
                }
            }
        }
        actions
    }

    /// Advances all animations and timers by `dt_s` seconds, emitting any completed
    /// actions (such as confirm-pick completion) into `actions_out`.
    ///
    /// Returns `true` if visual state changed and a redraw is needed.
    pub fn tick(&mut self, dt_s: f32, actions_out: &mut Vec<PickerActionOut>) -> bool {
        let dt = dt_s.clamp(0.0, 0.25);
        self.monotonic_s += dt;
        let mut changed = false;

        if self.drag_started && (self.monotonic_s - self.drag_last_sample_s) > 0.05 && dt > 0.0 {
            self.drag_velocity_px_s *= (-dt / 0.03).exp();
            if self.drag_velocity_px_s.abs() < 1.0 {
                self.drag_velocity_px_s = 0.0;
            }
        }

        if self.panel_progress.tick(dt) {
            changed = true;
        }
        if self.search_progress.tick(dt) {
            changed = true;
        }
        if !self.search_visible
            && !self.search_progress.is_animating()
            && self.search_progress.current <= 1e-4
            && !self.search_display_text.is_empty()
        {
            self.search_display_text.clear();
        }
        let scroll_moved = self.scroll_index.tick(dt);
        if scroll_moved {
            changed = true;
            if self.pointer_inside && !self.drag_started {
                self.update_hover_state();
            }
            self.sync_card_spring_targets();
        }
        if self.mode_pill_index.tick(dt) {
            changed = true;
        }
        if self.transition_pill_index.tick(dt) {
            changed = true;
        }
        for b in &mut self.mode_buttons {
            if b.tick(dt) {
                changed = true;
            }
        }
        for b in &mut self.transition_buttons {
            if b.tick(dt) {
                changed = true;
            }
        }
        for c in &mut self.card_anims {
            if c.width.tick(dt)
                | c.height.tick(dt)
                | c.visual_x_offset.tick(dt)
                | c.scale.tick(dt)
                | c.opacity.tick(dt)
                | c.hi_res_opacity.tick(dt)
            {
                changed = true;
            }
        }

        // Page switch animation & cooldown
        if self.page_animating {
            if self.page_progress.tick(dt) {
                changed = true;
            }
            if !self.page_progress.is_animating() {
                self.page_animating = false;
                self.outgoing_cards.clear();
                self.page_cooldown_s = PAGE_COOLDOWN_S;
                changed = true;
            }
        } else if self.page_cooldown_s > 0.0 {
            self.page_cooldown_s = (self.page_cooldown_s - dt).max(0.0);
        }

        // 1.5s hold expand preview timer
        if self.visible
            && !self.closing
            && self.hold_armed
            && !self.drag_started
            && self.held_index.is_none()
            && self.confirming_index.is_none()
            && !self.page_animating
            && !self.active_entries.is_empty()
        {
            self.hold_elapsed_s += dt;
            if self.hold_elapsed_s >= self.config.hold_delay_s {
                self.held_index = Some(self.current_index);
                self.hold_armed = false;
                self.sync_card_targets(false);
                changed = true;
            }
        }

        // Search bar blinking cursor
        if self.visible && !self.closing && self.search_visible && self.confirming_index.is_none() {
            let prev_vis = self.cursor_visible();
            self.cursor_blink_s += dt;
            if self.cursor_visible() != prev_vis {
                changed = true;
            }
        }

        // Panel close animation completion -> hide & emit Exit
        if self.closing && !self.panel_progress.is_animating() {
            self.closing = false;
            self.visible = false;
            self.panel_progress.snap(0.0);
            self.search_visible = false;
            self.search_progress.snap(0.0);
            self.search_display_text.clear();
            self.page_animating = false;
            self.outgoing_cards.clear();
            self.page_progress.snap(1.0);
            self.confirming_index = None;
            self.pending_pick_path = None;
            self.confirm_elapsed_s = 0.0;
            self.held_index = None;
            self.hold_armed = false;
            self.hi_res_ready_path = None;
            changed = true;
            actions_out.push(PickerActionOut::Exit);
        }

        // Confirm pick animation completion -> emit Apply + Exit
        if self.confirming_index.is_some() {
            self.confirm_elapsed_s += dt;
            changed = true;
            if self.confirm_elapsed_s >= CONFIRM_DURATION_S {
                let picked = self.pending_pick_path.take();
                self.confirming_index = None;
                self.confirm_elapsed_s = 0.0;
                self.visible = false;
                self.closing = false;
                self.panel_progress.snap(0.0);
                self.search_visible = false;
                self.search_progress.snap(0.0);
                self.search_display_text.clear();
                self.held_index = None;
                self.hi_res_ready_path = None;
                if let Some(path) = picked
                    && !path.is_empty()
                {
                    match self.page {
                        Page::Static => actions_out.push(PickerActionOut::ApplyStatic(path)),
                        Page::Video => {
                            actions_out.push(PickerActionOut::ApplyVideo(self.daemon.video_mode, path))
                        }
                    }
                }
                actions_out.push(PickerActionOut::Exit);
            }
        }

        if changed {
            self.dirty = true;
        }
        changed || self.dirty
    }

    /// Computes resolved layout for all active-page cards sorted back-to-front by z-order.
    pub fn card_layouts(&self) -> Vec<CardLayout> {
        let mut cards = Vec::with_capacity(self.active_entries.len());
        let cy = self.viewport_h * 0.5;
        let center_x_origin = self.viewport_w * 0.5;
        let max_idx = self.active_entries.len().saturating_sub(1);
        let center_idx = self
            .scroll_index
            .current
            .round()
            .clamp(0.0, max_idx as f32) as usize;
        for (i, entry) in self.active_entries.iter().enumerate() {
            let anim = &self.card_anims[i];
            let dist = (i as f32 - self.scroll_index.current).abs().round() as usize;
            let is_current = i == self.current_index;
            let is_confirmed = self.confirming_index == Some(i);
            let is_held = self.held_index == Some(i);
            let is_hovered = !self.drag_started
                && self.hovered_card == Some(i)
                && self.confirming_index.is_none();
            let z_order = if is_confirmed {
                100
            } else if i == center_idx {
                10
            } else {
                (10 - dist as i32).max(1)
            };
            let base_x = center_x_origin + (i as f32 - self.scroll_index.current) * self.config.item_width;
            let center_x = base_x + anim.visual_x_offset.current;
            cards.push(CardLayout {
                index: i,
                file_name: entry.file_name.clone(),
                thumbnail_path: entry.thumbnail_path.clone(),
                original_path: entry.original_path.clone(),
                center_x,
                center_y: cy,
                width: anim.width.current,
                height: anim.height.current,
                scale: anim.scale.current.max(0.05),
                opacity: anim.opacity.current.clamp(0.0, 1.0),
                hi_res_opacity: anim.hi_res_opacity.current,
                visual_x_offset: anim.visual_x_offset.current,
                dist_from_center: dist,
                z_order,
                is_current,
                is_held,
                is_confirmed,
                is_hovered,
            });
        }
        let scroll = self.scroll_index.current;
        cards.sort_by(|a, b| {
            a.z_order
                .cmp(&b.z_order)
                .then_with(|| {
                    let da = (a.index as f32 - scroll).abs();
                    let db = (b.index as f32 - scroll).abs();
                    db.total_cmp(&da)
                })
                .then_with(|| a.index.cmp(&b.index))
        });
        cards
    }

    /// Computes the logical rectangle of the top search box (`Carousel.qml::searchBar`).
    pub fn search_box_rect(&self) -> ControlRect {
        let width = 360.0_f32.min((self.viewport_w - 32.0).max(160.0));
        let height = 40.0;
        ControlRect {
            x: (self.viewport_w - width) * 0.5,
            y: 24.0,
            width,
            height,
        }
    }

    /// Computes the exact layout of both bottom `SkewSelector` controls.
    pub fn selector_layout(&self) -> SelectorLayout {
        let rail_h = 38.0_f32;
        let mode_btn_w = 122.0_f32;
        let mode_gap = 10.0_f32;
        let mode_rail_w = 2.0 * mode_btn_w + mode_gap;
        let mode_step = mode_btn_w + mode_gap;

        let trans_count = TRANSITION_CHOICES.len() as f32;
        let trans_gap = 8.0_f32;
        let trans_btn_w = 80.0_f32
            .min((self.viewport_w - 96.0 - (trans_count - 1.0) * trans_gap) / trans_count)
            .max(40.0);
        let trans_rail_w = trans_count * trans_btn_w + (trans_count - 1.0) * trans_gap;
        let trans_step = trans_btn_w + trans_gap;

        let combined_w = mode_rail_w + 40.0 + trans_rail_w;
        let controls_w = (self.viewport_w - 64.0).max(mode_rail_w).min(combined_w);
        let stacked = controls_w < combined_w - 0.5;
        let controls_h = if stacked {
            rail_h + 18.0 + rail_h
        } else {
            rail_h
        };
        let bottom_margin = (self.viewport_h * 0.08 - 16.0).clamp(32.0, 88.0);
        let controls_x = (self.viewport_w - controls_w) * 0.5;
        let controls_y = self.viewport_h - bottom_margin - controls_h;

        let mode_x = controls_x
            + if stacked {
                (controls_w - mode_rail_w) * 0.5
            } else {
                0.0
            };
        let mode_y = controls_y;
        let trans_x = controls_x
            + if stacked {
                (controls_w - trans_rail_w) * 0.5
            } else {
                mode_rail_w + 40.0
            };
        let trans_y = controls_y + if stacked { rail_h + 18.0 } else { 0.0 };

        let mode_available = self.is_mode_available();
        let active_mode_idx = self.active_mode_index();
        let active_trans_idx = self.active_transition_index();

        let mode_meta: [(&'static str, ModeIcon); 2] =
            [("普通", ModeIcon::Frame), ("视差", ModeIcon::Layers)];

        let mode_buttons = std::array::from_fn(|i| {
            let (label, icon) = mode_meta[i];
            let anim = &self.mode_buttons[i];
            ModeButtonLayout {
                index: i,
                label,
                icon,
                center_x: mode_x + (i as f32) * mode_step + mode_btn_w * 0.5,
                center_y: mode_y + rail_h * 0.5,
                width: mode_btn_w,
                height: rail_h,
                scale: anim.scale.current,
                bg_alpha: anim.bg_alpha.current,
                content_opacity: anim.content_opacity.current,
                selected: i == active_mode_idx,
                hovered: self.hovered_mode_button == Some(i),
            }
        });

        let transition_buttons = TRANSITION_CHOICES
            .iter()
            .enumerate()
            .map(|(i, choice)| {
                let anim = &self.transition_buttons[i];
                TransitionButtonLayout {
                    index: i,
                    transition: choice.value,
                    label: choice.label,
                    center_x: trans_x + (i as f32) * trans_step + trans_btn_w * 0.5,
                    center_y: trans_y + rail_h * 0.5,
                    width: trans_btn_w,
                    height: rail_h,
                    scale: anim.scale.current,
                    bg_alpha: anim.bg_alpha.current,
                    content_opacity: anim.content_opacity.current,
                    selected: i == active_trans_idx,
                    hovered: self.hovered_transition_button == Some(i),
                }
            })
            .collect();

        SelectorLayout {
            stacked,
            mode_available,
            mode_opacity: if mode_available { 1.0 } else { 0.4 },
            mode_pill_center_x: mode_x + self.mode_pill_index.current * mode_step + mode_btn_w * 0.5,
            mode_pill_center_y: mode_y + rail_h * 0.5,
            mode_pill_width: mode_btn_w,
            mode_pill_height: rail_h,
            mode_buttons,
            transition_pill_center_x: trans_x
                + self.transition_pill_index.current * trans_step
                + trans_btn_w * 0.5,
            transition_pill_center_y: trans_y + rail_h * 0.5,
            transition_pill_width: trans_btn_w,
            transition_pill_height: rail_h,
            transition_buttons,
        }
    }

    /// Hit-tests the top search box in logical viewport coordinates.
    pub fn hit_test_search_box(&self, x: f32, y: f32) -> bool {
        self.visible
            && !self.closing
            && self.search_visible
            && self.search_box_rect().contains(x, y)
    }

    /// Hit-tests the skewed mode selector buttons (`0` = 普通, `1` = 视差).
    pub fn hit_test_mode_button(&self, x: f32, y: f32) -> Option<usize> {
        if !self.visible || self.closing {
            return None;
        }
        let layout = self.selector_layout();
        for btn in &layout.mode_buttons {
            if point_in_skewed_rect(
                x,
                y,
                btn.center_x,
                btn.center_y,
                btn.width * btn.scale,
                btn.height * btn.scale,
                self.config.skew_factor,
            ) {
                return Some(btn.index);
            }
        }
        None
    }

    /// Hit-tests the 7 skewed transition selector buttons (`0..7`).
    pub fn hit_test_transition_button(&self, x: f32, y: f32) -> Option<usize> {
        if !self.visible || self.closing {
            return None;
        }
        let layout = self.selector_layout();
        for btn in &layout.transition_buttons {
            if point_in_skewed_rect(
                x,
                y,
                btn.center_x,
                btn.center_y,
                btn.width * btn.scale,
                btn.height * btn.scale,
                self.config.skew_factor,
            ) {
                return Some(btn.index);
            }
        }
        None
    }

    /// Hit-tests skewed carousel cards in front-to-back z-order.
    pub fn hit_test_card(&self, x: f32, y: f32) -> Option<usize> {
        if !self.visible || self.closing || self.page_animating || self.confirming_index.is_some() {
            return None;
        }
        let cards = self.card_layouts();
        for card in cards.iter().rev() {
            if card.opacity <= 0.01 {
                continue;
            }
            if point_in_skewed_rect(
                x,
                y,
                card.center_x,
                card.center_y,
                card.width * card.scale,
                card.height * card.scale,
                self.config.skew_factor,
            ) {
                return Some(card.index);
            }
        }
        None
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn page_entries(&self, page: Page) -> &[MediaEntry] {
        match page {
            Page::Static => &self.images,
            Page::Video => &self.videos,
        }
    }

    fn active_current_path(&self) -> &str {
        match self.page {
            Page::Static => &self.daemon.current_wallpaper,
            Page::Video => &self.daemon.current_video,
        }
    }

    fn is_mode_available(&self) -> bool {
        match self.page {
            Page::Static => !self.daemon.current_wallpaper.is_empty(),
            Page::Video => !self.daemon.current_video.is_empty(),
        }
    }

    fn active_mode_index(&self) -> usize {
        let is_parallax = match self.page {
            Page::Static => self.daemon.wallpaper_mode == WallpaperMode::Parallax,
            Page::Video => self.daemon.video_mode == VideoMode::Parallax,
        };
        usize::from(is_parallax)
    }

    fn active_transition_index(&self) -> usize {
        TRANSITION_CHOICES
            .iter()
            .position(|c| c.value == self.daemon.transition_type)
            .unwrap_or(0)
    }

    fn reset_drag_state(&mut self) {
        self.left_button_down = false;
        self.press_hit_control = false;
        self.drag_eligible = false;
        self.drag_started = false;
        self.drag_velocity_px_s = 0.0;
    }

    fn initial_page_for_engine(engine: EngineKind) -> Page {
        match engine {
            EngineKind::Video => Page::Video,
            EngineKind::Full | EngineKind::Simple => Page::Static,
        }
    }

    fn dismiss_search(&mut self, duration_s: f32) {
        if self.search_visible {
            if !self.search_text.is_empty() {
                self.search_display_text = self.search_text.clone();
            } else {
                self.search_display_text.clear();
            }
            self.search_visible = false;
            self.cursor_blink_s = 0.0;
            self.search_progress
                .set_target(0.0, duration_s, EasingKind::OutCubic);
            self.dirty = true;
        } else if self.search_progress.target > 0.0 {
            self.search_progress
                .set_target(0.0, duration_s, EasingKind::OutCubic);
            self.dirty = true;
        }
    }

    fn start_open(&mut self, actions: &mut Vec<PickerActionOut>) {
        if self.closing {
            // Cleanly interrupt an in-progress close without jumping or resetting cards/selection.
            self.closing = false;
            self.visible = true;
            self.panel_progress
                .set_target(1.0, PANEL_OPEN_DURATION_S, EasingKind::OutCubic);
            self.reset_hold_timer();
            if self.pointer_inside {
                self.update_hover_state();
            }
            if self.images.is_empty() || self.videos.is_empty() {
                actions.push(PickerActionOut::RequestCatalog);
            }
            self.dirty = true;
            return;
        }
        if self.visible {
            self.panel_progress
                .set_target(1.0, PANEL_OPEN_DURATION_S, EasingKind::OutCubic);
            self.dirty = true;
            return;
        }

        self.visible = true;
        self.closing = false;
        self.page = Self::initial_page_for_engine(self.daemon.engine);
        self.reset_drag_state();
        self.remembered_selection = [None, None];
        self.search_text.clear();
        self.search_display_text.clear();
        self.search_visible = false;
        self.search_progress.snap(0.0);
        self.confirming_index = None;
        self.pending_pick_path = None;
        self.confirm_elapsed_s = 0.0;
        self.page_animating = false;
        self.outgoing_cards.clear();
        self.page_progress.snap(1.0);
        self.wheel_accum = 0.0;
        self.panel_progress.snap(0.0);
        self.panel_progress
            .set_target(1.0, PANEL_OPEN_DURATION_S, EasingKind::OutCubic);
        self.rebuild_active_entries(true);
        self.sync_selector_targets(true);
        if self.images.is_empty() || self.videos.is_empty() {
            actions.push(PickerActionOut::RequestCatalog);
        }
        self.dirty = true;
    }

    fn start_close(&mut self, actions: &mut Vec<PickerActionOut>) {
        if !self.visible && !self.closing {
            actions.push(PickerActionOut::Exit);
            return;
        }
        if self.closing {
            return;
        }

        self.closing = true;
        self.reset_drag_state();
        let had_held = self.held_index.take().is_some();
        self.hold_armed = false;
        self.hold_elapsed_s = 0.0;
        self.hi_res_ready_path = None;
        let had_hover = self.hovered_card.take().is_some()
            || self.hovered_mode_button.take().is_some()
            || self.hovered_transition_button.take().is_some();
        if had_held || had_hover {
            self.sync_card_targets(false);
            self.sync_selector_targets(false);
        }
        self.dismiss_search(SEARCH_PANEL_CLOSE_DURATION_S);
        self.panel_progress
            .set_target(0.0, PANEL_CLOSE_DURATION_S, EasingKind::OutCubic);
        self.dirty = true;
    }

    fn rebuild_active_entries(&mut self, snap_scroll_and_cards: bool) {
        self.reset_drag_state();
        let preferred = self.remembered_selection[self.page.index()].as_deref();
        let current_path = self.active_current_path().to_string();
        let filtered = filter_entries(self.page_entries(self.page), &self.search_text);
        let idx = selected_index(&filtered, preferred, &current_path).unwrap_or(0);
        self.active_entries = filtered;
        self.current_index = idx;
        self.held_index = None;
        self.hi_res_ready_path = None;
        self.reset_hold_timer();
        self.hovered_card = None;

        if snap_scroll_and_cards || self.card_anims.len() != self.active_entries.len() {
            self.scroll_index.snap(self.current_index as f32);
            self.card_anims = (0..self.active_entries.len())
                .map(|i| self.initial_card_anim(i))
                .collect();
        } else {
            self.scroll_index.set_target(
                self.current_index as f32,
                SCROLL_DURATION_S,
                EasingKind::OutCubic,
            );
            self.sync_card_targets(false);
        }

        if self.pointer_inside {
            self.update_hover_state();
        }
    }

    fn initial_card_anim(&self, index: usize) -> CardAnimState {
        let (w, h, offset, scale, opacity, hi_res) = self.compute_card_targets(index);
        CardAnimState {
            width: AnimFloat::new(w),
            height: AnimFloat::new(h),
            visual_x_offset: AnimFloat::new(offset),
            scale: SmoothSpring::new(scale, CARD_SCALE_SPRING_OMEGA),
            opacity: SmoothSpring::new(opacity, CARD_OPACITY_SPRING_OMEGA),
            hi_res_opacity: AnimFloat::new(hi_res),
        }
    }

    fn compute_card_targets(&self, index: usize) -> (f32, f32, f32, f32, f32, f32) {
        let dist = (index as f32 - self.scroll_index.current).abs();
        let falloff = 1.0 / (1.0 + dist * dist);
        let is_current = index == self.current_index;
        let is_held = self.held_index == Some(index);
        let is_confirmed = self.confirming_index == Some(index);
        let is_other_confirming = self.confirming_index.is_some() && !is_confirmed;
        let is_hovered = !self.drag_started
            && self.hovered_card == Some(index)
            && self.confirming_index.is_none();

        let target_w = if is_held {
            self.viewport_w * self.config.hold_expand_ratio
        } else if self.config.expand_selected && is_current {
            self.config.item_width * self.config.expand_multiplier
        } else {
            self.config.item_width
        };
        let target_h = if is_held {
            self.viewport_h * self.config.hold_expand_ratio
        } else {
            self.config.item_height
        };

        let visual_x_offset = if (!self.config.expand_selected && self.held_index.is_none())
            || is_held
            || index == self.current_index
        {
            0.0
        } else {
            let space = if self.held_index.is_some() {
                self.viewport_w * self.config.hold_expand_ratio - self.config.item_width
            } else {
                self.config.item_width * self.config.expand_multiplier - self.config.item_width
            };
            if index > self.current_index {
                space * 0.5
            } else {
                -space * 0.5
            }
        };

        let base_scale = self.config.base_scale;
        let scale_range = self.config.selected_scale - base_scale;
        let target_scale = if is_confirmed {
            1.6
        } else if is_other_confirming {
            (base_scale + scale_range * falloff) * 0.8
        } else if is_hovered {
            base_scale + (scale_range + 0.20) * falloff
        } else {
            base_scale + scale_range * falloff
        };

        let target_opacity = if is_confirmed || is_other_confirming {
            0.0
        } else if is_hovered {
            1.0
        } else {
            0.1 + 0.9 * falloff
        };

        let target_hi_res = if is_held
            && self.page == Page::Static
            && let Some(entry) = self.active_entries.get(index)
            && self.hi_res_ready_path.as_deref() == Some(entry.original_path.as_str())
        {
            1.0
        } else {
            0.0
        };

        (
            target_w,
            target_h,
            visual_x_offset,
            target_scale,
            target_opacity,
            target_hi_res,
        )
    }

    fn sync_card_spring_targets(&mut self) {
        for i in 0..self.card_anims.len() {
            let (_, _, _, scale, opacity, _) = self.compute_card_targets(i);
            let anim = &mut self.card_anims[i];
            anim.scale.set_target(scale);
            anim.opacity.set_target(opacity);
        }
    }

    fn sync_card_targets(&mut self, snap: bool) {
        for i in 0..self.card_anims.len() {
            let (w, h, offset, scale, opacity, hi_res) = self.compute_card_targets(i);
            let anim = &mut self.card_anims[i];
            if snap {
                anim.width.snap(w);
                anim.height.snap(h);
                anim.visual_x_offset.snap(offset);
                anim.scale.snap(scale);
                anim.opacity.snap(opacity);
                anim.hi_res_opacity.snap(hi_res);
            } else {
                anim.width
                    .set_target(w, HOLD_EXPAND_DURATION_S, EasingKind::InOutCubic);
                anim.height
                    .set_target(h, HOLD_EXPAND_DURATION_S, EasingKind::InOutCubic);
                anim.visual_x_offset.set_target(
                    offset,
                    HOLD_EXPAND_DURATION_S,
                    EasingKind::InOutCubic,
                );
                anim.scale.set_target(scale);
                anim.opacity.set_target(opacity);
                if hi_res == 0.0 {
                    // Release immediately when unheld.
                    anim.hi_res_opacity.snap(0.0);
                } else {
                    anim.hi_res_opacity
                        .set_target(1.0, HIRES_FADE_DURATION_S, EasingKind::InOutQuad);
                }
            }
        }
    }

    fn sync_selector_targets(&mut self, snap: bool) {
        let mode_idx = self.active_mode_index();
        let trans_idx = self.active_transition_index();
        if snap {
            self.mode_pill_index.snap(mode_idx as f32);
            self.transition_pill_index.snap(trans_idx as f32);
        } else {
            self.mode_pill_index
                .set_target(mode_idx as f32, PILL_SLIDE_DURATION_S, EasingKind::OutCubic);
            self.transition_pill_index.set_target(
                trans_idx as f32,
                PILL_SLIDE_DURATION_S,
                EasingKind::OutCubic,
            );
        }
        let mode_available = self.is_mode_available();
        let busy = self.daemon.busy;
        let hovered_mode = self.hovered_mode_button;
        let hovered_trans = self.hovered_transition_button;
        for (i, b) in self.mode_buttons.iter_mut().enumerate() {
            let selected = i == mode_idx;
            let hovered = hovered_mode == Some(i) && mode_available && !busy;
            if snap {
                b.scale.snap(if hovered && !selected { 1.035 } else { 1.0 });
                b.bg_alpha.snap(if hovered && !selected {
                    0x12 as f32 / 255.0
                } else {
                    0x05 as f32 / 255.0
                });
                b.content_opacity.snap(if selected {
                    1.0
                } else if hovered {
                    0.8
                } else {
                    0.46
                });
            } else {
                b.update_targets(selected, hovered);
            }
        }
        for (i, b) in self.transition_buttons.iter_mut().enumerate() {
            let selected = i == trans_idx;
            let hovered = hovered_trans == Some(i) && !busy;
            if snap {
                b.scale.snap(if hovered && !selected { 1.035 } else { 1.0 });
                b.bg_alpha.snap(if hovered && !selected {
                    0x12 as f32 / 255.0
                } else {
                    0x05 as f32 / 255.0
                });
                b.content_opacity.snap(if selected {
                    1.0
                } else if hovered {
                    0.8
                } else {
                    0.46
                });
            } else {
                b.update_targets(selected, hovered);
            }
        }
    }

    fn reset_hold_timer(&mut self) {
        self.hold_elapsed_s = 0.0;
        self.hold_armed = self.config.enable_hold_expand && !self.active_entries.is_empty();
    }

    fn set_current_index(&mut self, next: usize) {
        if self.active_entries.is_empty() {
            return;
        }
        let clamped = next.min(self.active_entries.len() - 1);
        if clamped == self.current_index && self.held_index.is_none() {
            return;
        }
        self.current_index = clamped;
        self.held_index = None;
        self.hi_res_ready_path = None;
        self.reset_hold_timer();
        if let Some(entry) = self.active_entries.get(self.current_index) {
            self.remembered_selection[self.page.index()] = Some(entry.file_name.clone());
        }
        self.scroll_index.set_target(
            self.current_index as f32,
            SCROLL_DURATION_S,
            EasingKind::OutCubic,
        );
        self.sync_card_targets(false);
        self.dirty = true;
    }

    fn step_selection(&mut self, delta: isize) {
        if self.closing
            || self.active_entries.is_empty()
            || self.confirming_index.is_some()
            || self.page_animating
        {
            return;
        }
        let len = self.active_entries.len();
        let next = if delta > 0 {
            if self.current_index + 1 < len {
                self.current_index + 1
            } else if self.config.carousel_mode == CarouselMode::Wrap {
                0
            } else {
                self.current_index
            }
        } else if self.current_index > 0 {
            self.current_index - 1
        } else if self.config.carousel_mode == CarouselMode::Wrap {
            len - 1
        } else {
            0
        };
        self.set_current_index(next);
    }

    fn request_page_switch(&mut self, target: Page) -> bool {
        if target == self.page
            || !self.visible
            || self.closing
            || self.page_animating
            || self.page_cooldown_s > 0.0
            || self.confirming_index.is_some()
            || self.daemon.busy
        {
            return false;
        }
        if let Some(entry) = self.current_entry() {
            self.remembered_selection[self.page.index()] = Some(entry.file_name.clone());
        }
        self.outgoing_cards = self.card_layouts();
        self.page_direction = if target == Page::Video { -1.0 } else { 1.0 };
        self.page = target;
        self.dismiss_search(SEARCH_CLOSE_DURATION_S);
        self.search_text.clear();
        self.page_animating = true;
        self.page_progress.snap(0.0);
        self.page_progress
            .set_target(1.0, PAGE_SWITCH_DURATION_S, EasingKind::OutCubic);
        self.rebuild_active_entries(true);
        self.sync_selector_targets(false);
        self.dirty = true;
        true
    }

    fn confirm_pick(&mut self, index: usize) {
        if self.closing
            || self.confirming_index.is_some()
            || self.page_animating
            || self.daemon.busy
            || index >= self.active_entries.len()
        {
            return;
        }
        self.reset_drag_state();
        let path = self.active_entries[index].original_path.clone();
        self.confirming_index = Some(index);
        self.pending_pick_path = Some(path);
        self.confirm_elapsed_s = 0.0;
        self.held_index = None;
        self.hi_res_ready_path = None;
        self.hold_armed = false;
        self.sync_card_targets(false);
        self.dirty = true;
    }

    fn handle_key(&mut self, key: KeyInput, actions: &mut Vec<PickerActionOut>) {
        if !self.visible || self.closing || self.confirming_index.is_some() {
            return;
        }
        if key.modifiers.ctrl && matches!(key.key, KeyCode::Char('f' | 'F')) {
            self.search_visible = true;
            self.search_display_text.clear();
            self.cursor_blink_s = 0.0;
            self.search_progress
                .set_target(1.0, SEARCH_OPEN_DURATION_S, EasingKind::OutCubic);
            self.dirty = true;
            return;
        }

        if self.search_visible {
            match key.key {
                KeyCode::Escape => {
                    if !self.search_text.is_empty() {
                        self.search_text.clear();
                        self.rebuild_active_entries(true);
                        self.cursor_blink_s = 0.0;
                        self.dirty = true;
                    } else {
                        self.dismiss_search(SEARCH_CLOSE_DURATION_S);
                    }
                }
                KeyCode::Backspace => {
                    if self.search_text.pop().is_some() {
                        self.cursor_blink_s = 0.0;
                        self.rebuild_active_entries(true);
                        self.dirty = true;
                    }
                }
                KeyCode::Left => self.step_selection(-1),
                KeyCode::Right => self.step_selection(1),
                KeyCode::Enter => self.confirm_pick(self.current_index),
                KeyCode::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt && !c.is_control() => {
                    self.search_text.push(c);
                    self.cursor_blink_s = 0.0;
                    self.rebuild_active_entries(true);
                    self.dirty = true;
                }
                _ => {}
            }
            return;
        }

        match key.key {
            KeyCode::Escape => self.start_close(actions),
            KeyCode::Left | KeyCode::Char('h' | 'H') => self.step_selection(-1),
            KeyCode::Right | KeyCode::Char('l' | 'L') => self.step_selection(1),
            KeyCode::Home => self.set_current_index(0),
            KeyCode::End => {
                if !self.active_entries.is_empty() {
                    self.set_current_index(self.active_entries.len() - 1);
                }
            }
            KeyCode::Enter => self.confirm_pick(self.current_index),
            _ => {}
        }
    }

    fn is_in_carousel_band(&self, x: f32, y: f32) -> bool {
        if !self.visible
            || self.closing
            || self.active_entries.is_empty()
            || self.page_animating
            || self.confirming_index.is_some()
        {
            return false;
        }
        if self.hit_test_search_box(x, y)
            || self.hit_test_mode_button(x, y).is_some()
            || self.hit_test_transition_button(x, y).is_some()
        {
            return false;
        }
        if self.hit_test_card(x, y).is_some() {
            return true;
        }
        let cy = self.viewport_h * 0.5;
        let card_half_h = (self.config.item_height * self.config.selected_scale * 0.5)
            .max(self.viewport_h * self.config.hold_expand_ratio * 0.5)
            + 32.0;
        let sel = self.selector_layout();
        let sel_top = sel.mode_pill_center_y - sel.mode_pill_height * 0.5 - 12.0;
        let search_bottom = if self.search_visible {
            let sb = self.search_box_rect();
            sb.y + sb.height + 12.0
        } else {
            0.0
        };
        (y - cy).abs() <= card_half_h && y < sel_top && y > search_bottom
    }

    fn handle_pointer_press(&mut self, x: f32, y: f32, button: MouseButton) {
        self.pointer_x = x;
        self.pointer_y = y;
        self.pointer_inside = true;
        self.update_hover_state();

        if button != MouseButton::Left
            || !self.visible
            || self.closing
            || self.confirming_index.is_some()
            || self.page_animating
        {
            return;
        }

        let hit_control = self.hit_test_search_box(x, y)
            || self.hit_test_mode_button(x, y).is_some()
            || self.hit_test_transition_button(x, y).is_some();

        self.left_button_down = true;
        self.press_x = x;
        self.press_y = y;
        self.press_hit_control = hit_control;
        self.drag_started = false;
        self.drag_velocity_px_s = 0.0;

        if !hit_control && self.is_in_carousel_band(x, y) {
            self.drag_eligible = true;
            self.drag_anchor_x = x;
            self.drag_start_scroll = self.scroll_index.current;
            self.drag_start_index = self.current_index;
            self.drag_last_x = x;
            self.drag_last_sample_s = self.monotonic_s;
        } else {
            self.drag_eligible = false;
        }
    }

    fn handle_pointer_move(&mut self, x: f32, y: f32) {
        self.pointer_x = x;
        self.pointer_y = y;
        self.pointer_inside = true;

        if self.left_button_down
            && self.drag_eligible
            && self.visible
            && !self.closing
            && self.confirming_index.is_none()
            && !self.page_animating
            && !self.active_entries.is_empty()
        {
            if !self.drag_started
                && (x - self.press_x).abs() > DRAG_THRESHOLD_PX
                && (x - self.press_x).abs() >= (y - self.press_y).abs() * 0.5
            {
                self.drag_started = true;
                if self.scroll_index.is_animating()
                    || (self.scroll_index.current - self.drag_start_scroll).abs() > 1e-4
                {
                    self.drag_start_scroll = self.scroll_index.current;
                    self.drag_start_index = self.current_index;
                    self.drag_anchor_x = x;
                    self.scroll_index.snap(self.drag_start_scroll);
                }
                self.hold_armed = false;
                self.hold_elapsed_s = 0.0;
                self.held_index = None;
                self.hi_res_ready_path = None;
                self.hovered_card = None;
                self.drag_velocity_px_s = 0.0;
                self.drag_last_x = x;
                self.drag_last_sample_s = self.monotonic_s;
                self.sync_card_targets(false);
            }

            if self.drag_started {
                let dt = self.monotonic_s - self.drag_last_sample_s;
                let step_dx = x - self.drag_last_x;
                if dt > 1e-4 {
                    let inst_v = step_dx / dt;
                    let blend = (dt / 0.04).clamp(0.15, 0.75);
                    self.drag_velocity_px_s =
                        self.drag_velocity_px_s * (1.0 - blend) + inst_v * blend;
                    self.drag_last_sample_s = self.monotonic_s;
                    self.drag_last_x = x;
                } else if step_dx.abs() > 0.0 {
                    self.drag_last_x = x;
                }

                let step = self.config.item_width.max(1.0);
                let dx = x - self.drag_anchor_x;
                let raw_scroll = self.drag_start_scroll - dx / step;
                let max_idx = self.active_entries.len().saturating_sub(1) as f32;
                let drag_scroll = if raw_scroll < 0.0 {
                    (raw_scroll * DRAG_RUBBER_BAND_FACTOR).max(-DRAG_RUBBER_BAND_MAX_CARDS)
                } else if raw_scroll > max_idx {
                    max_idx
                        + ((raw_scroll - max_idx) * DRAG_RUBBER_BAND_FACTOR)
                            .min(DRAG_RUBBER_BAND_MAX_CARDS)
                } else {
                    raw_scroll
                };
                self.scroll_index.snap(drag_scroll);
                let nearest = (drag_scroll.round() as isize).clamp(0, max_idx as isize) as usize;
                self.current_index = nearest;
                self.sync_card_spring_targets();
                self.dirty = true;
                return;
            }
        }

        self.update_hover_state();
    }

    fn handle_pointer_leave(&mut self) {
        self.pointer_inside = false;

        if self.drag_started {
            self.finish_drag_release();
        } else {
            self.reset_drag_state();
        }

        self.pointer_x = -1.0;
        self.pointer_y = -1.0;

        let mut changed = false;
        if self.hovered_card.take().is_some() {
            self.sync_card_targets(false);
            changed = true;
        }
        if self.hovered_mode_button.take().is_some() {
            changed = true;
        }
        if self.hovered_transition_button.take().is_some() {
            changed = true;
        }
        if changed {
            self.sync_selector_targets(false);
            self.dirty = true;
        }
    }

    fn handle_pointer_release(
        &mut self,
        x: f32,
        y: f32,
        button: MouseButton,
        actions: &mut Vec<PickerActionOut>,
    ) {
        self.pointer_x = x;
        self.pointer_y = y;
        self.pointer_inside = true;

        if self.closing {
            self.reset_drag_state();
            return;
        }

        if button != MouseButton::Left || !self.left_button_down {
            self.update_hover_state();
            return;
        }

        if self.drag_started {
            self.finish_drag_release();
            return;
        }

        let was_control = self.press_hit_control;
        self.reset_drag_state();
        self.update_hover_state();

        if was_control {
            let _ = self.handle_control_click(x, y, actions);
        } else {
            self.handle_left_click(x, y, actions);
        }
    }

    fn finish_drag_release(&mut self) {
        self.left_button_down = false;
        self.drag_eligible = false;
        self.drag_started = false;
        self.press_hit_control = false;

        if self.active_entries.is_empty() {
            self.drag_velocity_px_s = 0.0;
            return;
        }

        let len = self.active_entries.len();
        let max_idx = (len - 1) as f32;
        let step = self.config.item_width.max(1.0);
        let v_cards_s = -self.drag_velocity_px_s / step;
        let momentum = (v_cards_s * FLICK_MOMENTUM_S).clamp(-FLICK_MAX_CARDS, FLICK_MAX_CARDS);
        let projected = self.scroll_index.current + momentum;
        self.drag_velocity_px_s = 0.0;

        let target_idx = if self.config.carousel_mode == CarouselMode::Wrap && len > 1 {
            let raw_dx = self.pointer_x - self.drag_anchor_x;
            let raw_end = self.drag_start_scroll - raw_dx / step + momentum;
            if self.drag_start_index == 0 && raw_end < -0.45 {
                len - 1
            } else if self.drag_start_index == len - 1 && raw_end > max_idx + 0.45 {
                0
            } else {
                (projected.round() as isize).clamp(0, max_idx as isize) as usize
            }
        } else {
            (projected.round() as isize).clamp(0, max_idx as isize) as usize
        };

        self.current_index = target_idx;
        self.held_index = None;
        self.hi_res_ready_path = None;
        self.reset_hold_timer();
        if let Some(entry) = self.active_entries.get(self.current_index) {
            self.remembered_selection[self.page.index()] = Some(entry.file_name.clone());
        }
        let dist = (target_idx as f32 - self.scroll_index.current).abs();
        let dur = (0.20 + dist * 0.04).clamp(0.20, 0.38);
        self.scroll_index
            .set_target(target_idx as f32, dur, EasingKind::OutCubic);
        if self.pointer_inside {
            self.update_hover_state();
        }
        self.sync_card_targets(false);
        self.dirty = true;
    }

    fn update_hover_state(&mut self) {
        if !self.visible || self.closing {
            return;
        }
        let x = self.pointer_x;
        let y = self.pointer_y;

        let over_search = self.hit_test_search_box(x, y);
        let next_mode = if over_search {
            None
        } else {
            self.hit_test_mode_button(x, y)
        };
        let next_trans = if over_search || next_mode.is_some() {
            None
        } else {
            self.hit_test_transition_button(x, y)
        };
        let next_card =
            if self.drag_started || over_search || next_mode.is_some() || next_trans.is_some() {
                None
            } else {
                self.hit_test_card(x, y)
            };

        let mut changed = false;
        if self.hovered_mode_button != next_mode {
            self.hovered_mode_button = next_mode;
            changed = true;
        }
        if self.hovered_transition_button != next_trans {
            self.hovered_transition_button = next_trans;
            changed = true;
        }
        if self.hovered_card != next_card {
            self.hovered_card = next_card;
            self.sync_card_targets(false);
            changed = true;
        }
        if changed {
            self.sync_selector_targets(false);
            self.dirty = true;
        }
    }

    fn handle_control_click(
        &mut self,
        x: f32,
        y: f32,
        actions: &mut Vec<PickerActionOut>,
    ) -> bool {
        if !self.visible || self.closing || self.confirming_index.is_some() {
            return true;
        }
        if self.hit_test_search_box(x, y) {
            return true;
        }
        if let Some(btn_idx) = self.hit_test_mode_button(x, y) {
            if !self.daemon.busy && self.is_mode_available() && btn_idx != self.active_mode_index()
            {
                actions.push(PickerActionOut::ToggleMode);
            }
            return true;
        }
        if let Some(btn_idx) = self.hit_test_transition_button(x, y) {
            if !self.daemon.busy && btn_idx != self.active_transition_index() {
                let choice = TRANSITION_CHOICES[btn_idx].value;
                self.daemon.transition_type = choice;
                self.sync_selector_targets(false);
                actions.push(PickerActionOut::SelectTransition(choice));
                self.dirty = true;
            }
            return true;
        }
        false
    }

    fn handle_left_click(&mut self, x: f32, y: f32, actions: &mut Vec<PickerActionOut>) {
        if !self.visible || self.closing || self.confirming_index.is_some() {
            return;
        }
        if self.handle_control_click(x, y, actions) {
            return;
        }
        if let Some(card_idx) = self.hit_test_card(x, y) {
            self.confirm_pick(card_idx);
            return;
        }
        // Clicked backdrop scrim outside interactive elements -> close picker.
        self.start_close(actions);
    }
}
