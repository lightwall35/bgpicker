//! EGL display/context/surface management for Wayland (`wl_egl_window`) and
//! headless offscreen GPU testing (`EGL_PLATFORM_SURFACELESS_MESA` / pbuffer).

use std::ffi::c_void;
use std::sync::Arc;

use glow::HasContext;
use khronos_egl as egl;

use crate::error::{GfxError, Result};

const EGL_PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;
const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;
const EGL_OPENGL_ES3_BIT: egl::Int = 0x0040;
const EGL_CONTEXT_MAJOR_VERSION: egl::Int = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: egl::Int = 0x3099;

type Egl15 = egl::Instance<egl::Static>;

/// Active EGL display + shared OpenGL (ES 3.0 or Desktop 3.3) context.
pub struct EglContext {
    egl: Egl15,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    fallback_surface: Option<egl::Surface>,
    surfaceless: bool,
    terminate_on_drop: bool,
    pub is_gles: bool,
    pub is_nvidia: bool,
    pub bgra_ext_supported: bool,
    pub gl: Arc<glow::Context>,
}

impl EglContext {
    /// Initializes an EGL context bound to a live Wayland `wl_display` pointer.
    ///
    /// # Safety
    /// `wl_display_ptr` must be a valid, open `wl_display` pointer that outlives this [`EglContext`].
    pub unsafe fn new_wayland(wl_display_ptr: *mut c_void) -> Result<Self> {
        if wl_display_ptr.is_null() {
            return Err(GfxError::Egl("null wl_display pointer".into()));
        }
        let egl = egl::Instance::new(egl::Static);
        let display = unsafe {
            egl.get_platform_display(
                EGL_PLATFORM_WAYLAND_KHR,
                wl_display_ptr,
                &[egl::ATTRIB_NONE],
            )
        }
        .or_else(|_| unsafe { egl.get_display(wl_display_ptr) }.ok_or(egl::Error::BadDisplay))
        .map_err(|e| GfxError::Egl(format!("eglGetDisplay(Wayland) failed: {e}")))?;

        Self::init_with_display(egl, display, Some(egl::WINDOW_BIT), false, true)
    }

    /// Initializes a headless offscreen EGL context (surfaceless Mesa or pbuffer) for
    /// shader/pipeline tests without requiring a Wayland compositor.
    pub fn new_headless() -> Result<Self> {
        let egl = egl::Instance::new(egl::Static);

        // Prefer surfaceless Mesa display if available; fall back to default display + pbuffer.
        if let Ok(display) = unsafe {
            egl.get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                egl::DEFAULT_DISPLAY,
                &[egl::ATTRIB_NONE],
            )
        } && let Ok(ctx) = Self::init_with_display(
            egl::Instance::new(egl::Static),
            display,
            None,
            false,
            false,
        ) {
            return Ok(ctx);
        }

        let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }
            .ok_or_else(|| GfxError::Egl("eglGetDisplay(DEFAULT_DISPLAY) returned null".into()))?;
        Self::init_with_display(egl, display, Some(egl::PBUFFER_BIT), true, false)
    }

    fn init_with_display(
        egl: Egl15,
        display: egl::Display,
        surface_type: Option<egl::Int>,
        allow_pbuffer_fallback: bool,
        terminate_on_drop: bool,
    ) -> Result<Self> {
        egl.initialize(display)
            .map_err(|e| GfxError::Egl(format!("eglInitialize failed: {e}")))?;

        let exts = egl
            .query_string(Some(display), egl::EXTENSIONS)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let surfaceless = exts.split_whitespace().any(|e| e == "EGL_KHR_surfaceless_context");

        // Try OpenGL ES 3.0 first, then Desktop OpenGL 3.3 Core.
        let (config, context, is_gles) =
            match create_gles3_context(&egl, display, surface_type) {
                Ok((cfg, ctx)) => (cfg, ctx, true),
                Err(gles_err) => match create_gl33_context(&egl, display, surface_type) {
                    Ok((cfg, ctx)) => (cfg, ctx, false),
                    Err(gl_err) => {
                        if terminate_on_drop {
                            let _ = egl.terminate(display);
                        }
                        return Err(GfxError::Egl(format!(
                            "failed to create GLES3 ({gles_err}) or GL3.3 ({gl_err}) context"
                        )));
                    }
                },
            };

        let fallback_surface = if !surfaceless || allow_pbuffer_fallback {
            if !surfaceless {
                let pbuf_attribs = [egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE];
                let surf = egl
                    .create_pbuffer_surface(display, config, &pbuf_attribs)
                    .map_err(|e| GfxError::Egl(format!("create_pbuffer_surface failed: {e}")))?;
                Some(surf)
            } else {
                None
            }
        } else {
            None
        };

        egl.make_current(
            display,
            fallback_surface,
            fallback_surface,
            Some(context),
        )
        .map_err(|e| GfxError::Egl(format!("initial eglMakeCurrent failed: {e}")))?;

        let gl = Arc::new(unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map(|f| f as *const c_void)
                    .unwrap_or(std::ptr::null())
            })
        });

        let bgra_ext_supported = if !is_gles {
            true
        } else {
            gl.supported_extensions()
                .iter()
                .any(|ext| ext == "GL_EXT_texture_format_BGRA8888" || ext == "GL_EXT_bgra")
        };

        let egl_vendor = egl
            .query_string(Some(display), egl::VENDOR)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let gl_vendor = unsafe { gl.get_parameter_string(glow::VENDOR) };
        let is_nvidia = egl_vendor.to_ascii_lowercase().contains("nvidia")
            || gl_vendor.to_ascii_lowercase().contains("nvidia");
        let _ver = unsafe { gl.get_parameter_string(glow::VERSION) };

        Ok(Self {
            egl,
            display,
            config,
            context,
            fallback_surface,
            surfaceless,
            terminate_on_drop,
            is_gles,
            is_nvidia,
            bgra_ext_supported,
            gl,
        })
    }

    /// Creates an `EGLSurface` for a `wl_egl_window` native window pointer.
    ///
    /// # Safety
    /// `native_window` must be a valid `wl_egl_window*` belonging to the same `wl_display`.
    pub unsafe fn create_window_surface(&self, native_window: *mut c_void) -> Result<egl::Surface> {
        let surf = unsafe {
            self.egl.create_platform_window_surface(
                self.display,
                self.config,
                native_window,
                &[egl::ATTRIB_NONE],
            )
        }
        .or_else(|_| unsafe {
            self.egl
                .create_window_surface(self.display, self.config, native_window, None)
        })
        .map_err(|e| GfxError::EglSurface(format!("{e}")))?;

        // Set swap interval 0 so multi-monitor rendering is paced by Wayland frame callbacks
        // rather than blocking inside eglSwapBuffers.
        if self
            .egl
            .make_current(self.display, Some(surf), Some(surf), Some(self.context))
            .is_ok()
        {
            let _ = self.egl.swap_interval(self.display, 0);
        }
        Ok(surf)
    }

    /// Makes `surface` current (or falls back to surfaceless/pbuffer when `None`).
    pub fn make_current(&self, surface: Option<egl::Surface>) -> Result<()> {
        let target = surface.or(self.fallback_surface);
        if target.is_none() && !self.surfaceless {
            return Err(GfxError::Egl("no current surface and surfaceless unsupported".into()));
        }
        self.egl
            .make_current(self.display, target, target, Some(self.context))
            .map_err(|e| GfxError::Egl(format!("eglMakeCurrent failed: {e}")))
    }

    /// Swaps the back buffer of `surface`.
    pub fn swap_buffers(&self, surface: egl::Surface) -> Result<()> {
        // Guard against `khronos-egl 6.0` panicking in `get_error().unwrap()` when a
        // compositor disconnects mid-shutdown (`EGL_FALSE` with `EGL_SUCCESS`).
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.egl.swap_buffers(self.display, surface)
        })) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(GfxError::Egl(format!("eglSwapBuffers failed: {e}"))),
            Err(_) => Err(GfxError::Egl("eglSwapBuffers failed on disconnected display".into())),
        }
    }

    /// Destroys an `EGLSurface` after detaching it from current if needed.
    pub fn destroy_surface(&self, surface: egl::Surface) {
        let _ = self.make_current(None);
        let _ = self.egl.destroy_surface(self.display, surface);
    }

    /// Ensures the fallback/surfaceless context is current before GL resource cleanup.
    pub fn make_fallback_current(&self) -> Result<()> {
        self.make_current(None)
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        if let Some(surf) = self.fallback_surface.take() {
            let _ = self.egl.destroy_surface(self.display, surf);
        }
        let _ = self.egl.destroy_context(self.display, self.context);
        if self.terminate_on_drop {
            let _ = self.egl.terminate(self.display);
        }
    }
}

fn build_config_attribs(surface_type: Option<egl::Int>, renderable: egl::Int) -> Vec<egl::Int> {
    let mut attribs = Vec::with_capacity(14);
    if let Some(st) = surface_type {
        attribs.push(egl::SURFACE_TYPE);
        attribs.push(st);
    }
    attribs.extend_from_slice(&[
        egl::RENDERABLE_TYPE,
        renderable,
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::ALPHA_SIZE,
        8,
        egl::NONE,
    ]);
    attribs
}

fn create_gles3_context(
    egl: &Egl15,
    display: egl::Display,
    surface_type: Option<egl::Int>,
) -> std::result::Result<(egl::Config, egl::Context), String> {
    egl.bind_api(egl::OPENGL_ES_API)
        .map_err(|e| format!("bind_api(GLES): {e}"))?;

    let attribs = build_config_attribs(surface_type, EGL_OPENGL_ES3_BIT);
    let config = egl
        .choose_first_config(display, &attribs)
        .map_err(|e| format!("choose_first_config: {e}"))?
        .ok_or_else(|| "no matching EGLConfig for GLES3".to_string())?;

    let ctx_attribs = [egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE];
    let ctx = egl
        .create_context(display, config, None, &ctx_attribs)
        .map_err(|e| format!("create_context(GLES3): {e}"))?;
    Ok((config, ctx))
}

fn create_gl33_context(
    egl: &Egl15,
    display: egl::Display,
    surface_type: Option<egl::Int>,
) -> std::result::Result<(egl::Config, egl::Context), String> {
    egl.bind_api(egl::OPENGL_API)
        .map_err(|e| format!("bind_api(GL): {e}"))?;

    let attribs = build_config_attribs(surface_type, egl::OPENGL_BIT);
    let config = egl
        .choose_first_config(display, &attribs)
        .map_err(|e| format!("choose_first_config: {e}"))?
        .ok_or_else(|| "no matching EGLConfig for GL".to_string())?;

    let ctx_attribs = [EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3, egl::NONE];
    let ctx = egl
        .create_context(display, config, None, &ctx_attribs)
        .map_err(|e| format!("create_context(GL3.3): {e}"))?;
    Ok((config, ctx))
}


