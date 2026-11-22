use crate::model::Enhancement;
use anyhow::{Context, Result};
use smithay::{
    backend::{
        egl::{
            EGLContext, EGLDisplay, EGLSurface,
            context::{GlAttributes, PixelFormatRequirements},
            ffi,
            native::{EGLNativeDisplay, EGLPlatform},
        },
        renderer::gles::GlesRenderer,
    },
    egl_platform,
    reexports::winit::{
        application::ApplicationHandler,
        dpi::PhysicalSize,
        event::WindowEvent,
        event_loop::{
            ActiveEventLoop, EventLoop, OwnedDisplayHandle, pump_events::EventLoopExtPumpEvents,
        },
        monitor::Fullscreen,
        platform::wayland::WindowAttributesWayland,
        raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle},
        window::{Window, WindowAttributes, WindowId},
    },
    utils::{Physical, Size},
};
use std::{
    os::fd::{AsFd, OwnedFd},
    time::Duration,
};

struct NativeDisplay(OwnedDisplayHandle);

impl EGLNativeDisplay for NativeDisplay {
    fn supported_platforms(&self) -> Vec<EGLPlatform<'_>> {
        match self.0.display_handle().map(|handle| handle.as_raw()) {
            Ok(RawDisplayHandle::Wayland(handle)) => {
                let display = handle.display.as_ptr();
                vec![
                    egl_platform!(PLATFORM_WAYLAND_KHR, display, &["EGL_KHR_platform_wayland"]),
                    egl_platform!(PLATFORM_WAYLAND_EXT, display, &["EGL_EXT_platform_wayland"]),
                ]
            }
            _ => Vec::new(),
        }
    }
}

pub(super) struct Desktop {
    event_loop: EventLoop,
    attributes: WindowAttributes,
    pub view: Option<DesktopView>,
}

impl Desktop {
    pub fn new(settings: &Enhancement) -> Result<(Self, GlesRenderer)> {
        let event_loop = EventLoop::builder().build()?;
        // The EGL display retains the Wayland connection independently of the window.
        let display = unsafe { EGLDisplay::new(NativeDisplay(event_loop.owned_display_handle()))? };
        let gl = GlAttributes {
            version: (3, 2),
            profile: None,
            debug: false,
            vsync: true,
        };
        let context = EGLContext::new_with_config(&display, gl, PixelFormatRequirements::_10_bit())
            .or_else(|_| {
                EGLContext::new_with_config(&display, gl, PixelFormatRequirements::_8_bit())
            })?;
        let renderer = unsafe { GlesRenderer::new(context)? };
        let mut desktop = Self {
            event_loop,
            attributes: WindowAttributes::default()
                .with_title("GalScaler — 启动中")
                .with_platform_attributes(Box::new(
                    WindowAttributesWayland::default()
                        .with_name("io.misari.galscaler", "GalScaler"),
                ))
                .with_surface_size(PhysicalSize::new(
                    settings.output.width,
                    settings.output.height,
                ))
                .with_fullscreen(settings.fullscreen.then_some(Fullscreen::Borderless(None))),
            view: None,
        };
        desktop.show(&renderer)?;
        Ok((desktop, renderer))
    }

    pub fn event_fd(&self) -> Result<OwnedFd> {
        Ok(self.event_loop.as_fd().try_clone_to_owned()?)
    }

    pub fn show(&mut self, renderer: &GlesRenderer) -> Result<()> {
        if self.view.is_some() {
            return Ok(());
        }
        let mut events = WindowEvents {
            attributes: Some(self.attributes.clone()),
            ..Default::default()
        };
        self.event_loop
            .pump_app_events(Some(Duration::ZERO), &mut events);
        let window = events.created.context("无法创建游戏窗口")??;
        let RawWindowHandle::Wayland(handle) = window.window_handle()?.as_raw() else {
            anyhow::bail!("游戏窗口需要 Wayland 桌面");
        };
        let size = window.surface_size();
        let context = renderer.egl_context();
        // Drop DesktopView's EGL surface before its winit window.
        let surface = unsafe {
            let native = wayland_egl::WlEglSurface::new_from_raw(
                handle.surface.as_ptr().cast(),
                size.width as i32,
                size.height as i32,
            )?;
            EGLSurface::new(
                context.display(),
                context.pixel_format().unwrap(),
                context.config_id(),
                native,
            )?
        };
        self.view = Some(DesktopView {
            surface,
            window,
            size: (size.width as i32, size.height as i32).into(),
        });
        Ok(())
    }

    pub fn hide(&mut self) {
        if let Some(view) = self.view.take() {
            let fullscreen = view.window.fullscreen();
            if fullscreen.is_none() {
                self.attributes.surface_size = Some(view.window.surface_size().into());
            }
            self.attributes.fullscreen = fullscreen;
            drop(view);
            self.events();
        }
    }

    pub fn events(&mut self) -> Vec<WindowEvent> {
        let mut events = WindowEvents {
            active: self.view.as_ref().map(|view| view.window.id()),
            ..Default::default()
        };
        self.event_loop
            .pump_app_events(Some(Duration::ZERO), &mut events);
        events.pending
    }
}

pub(super) struct DesktopView {
    pub surface: EGLSurface,
    pub window: Box<dyn Window>,
    size: Size<i32, Physical>,
}

impl DesktopView {
    pub fn size(&mut self) -> Size<i32, Physical> {
        let size = self.window.surface_size();
        let size: Size<i32, Physical> = (size.width as i32, size.height as i32).into();
        if size != self.size && size.w > 0 && size.h > 0 {
            self.surface.resize(size.w, size.h, 0, 0);
            self.size = size;
        }
        size
    }

    pub fn submit(&self) -> Result<()> {
        self.window.pre_present_notify();
        self.surface.swap_buffers(None)?;
        Ok(())
    }
}

#[derive(Default)]
struct WindowEvents {
    attributes: Option<WindowAttributes>,
    created: Option<Result<Box<dyn Window>, smithay::reexports::winit::error::RequestError>>,
    active: Option<WindowId>,
    pending: Vec<WindowEvent>,
}

impl ApplicationHandler for WindowEvents {
    fn can_create_surfaces(&mut self, _: &dyn ActiveEventLoop) {}

    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        if let Some(attributes) = self.attributes.take() {
            self.created = Some(event_loop.create_window(attributes));
        }
    }

    fn window_event(&mut self, _: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.active == Some(id) {
            self.pending.push(event);
        }
    }
}
