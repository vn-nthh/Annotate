#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::OnceLock;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::{Interface, Result as WindowsResult};
    use windows::System::DispatcherQueueController;
    use windows::Win32::Foundation::{
        COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush, DeleteDC,
        DeleteObject, DrawTextW, Ellipse, GetStockObject, GetTextExtentPoint32W, LineTo, MoveToEx,
        RoundRect, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
        DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_NOPREFIX,
        DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_MEDIUM, HGDIOBJ, NULL_BRUSH, OUT_DEFAULT_PRECIS,
        PS_SOLID, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
    use windows::Win32::System::WinRT::{
        CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, RoUninitialize,
        DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED,
    };
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics,
        KillTimer, PostQuitMessage, PostThreadMessageW, RegisterClassW, SetTimer, SetWindowPos,
        ShowWindow, TranslateMessage, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, HTTRANSPARENT,
        HWND_TOPMOST, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
        SW_SHOWNOACTIVATE, ULW_ALPHA, WM_DESTROY, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_TIMER,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };
    use windows::UI::Color;
    use windows::UI::Composition::Desktop::DesktopWindowTarget;
    use windows::UI::Composition::{
        CompositionRoundedRectangleGeometry, Compositor, ContainerVisual, SpriteVisual,
    };
    use windows_numerics::{Vector2, Vector3};

    const LISTEN_WIDTH: i32 = 72;
    const OVERLAY_HEIGHT: i32 = 32;
    const CONFIRM_CANVAS_WIDTH: i32 = 260;
    const BOTTOM_MARGIN: i32 = 60;
    const TIMER_ID: usize = 1;
    const WAKE_MESSAGE: u32 = 0x8000 + 0x41;

    static HANDLE: OnceLock<NativeThrobberHandle> = OnceLock::new();

    struct NativeThrobberHandle {
        sender: Sender<Command>,
        thread_id: u32,
    }

    enum Command {
        ShowListening,
        Hide,
        SetLevel(f32),
        ShowConfirmation {
            kind: ConfirmationKind,
            text: String,
        },
    }

    #[derive(Clone, Copy)]
    enum ConfirmationKind {
        Added,
        Duplicate,
    }

    struct CompositionScene {
        _dispatcher: DispatcherQueueController,
        _compositor: Compositor,
        _target: DesktopWindowTarget,
        _root: ContainerVisual,
        _pill: SpriteVisual,
        _pill_geometry: CompositionRoundedRectangleGeometry,
        dots: Vec<DotVisual>,
    }

    struct DotVisual {
        visual: SpriteVisual,
        geometry: CompositionRoundedRectangleGeometry,
    }

    struct OverlayState {
        listening_hwnd: HWND,
        confirmation_hwnd: HWND,
        scene: CompositionScene,
        scale: f32,
        visible: bool,
        level: f32,
        smoothed_level: f32,
        last_level_at: Instant,
        animation_started_at: Instant,
        animations_enabled: bool,
    }

    pub fn initialize() -> Result<(), String> {
        if HANDLE.get().is_some() {
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

        thread::Builder::new()
            .name("annotate-native-throbber".to_string())
            .spawn(move || thread_main(receiver, ready_sender))
            .map_err(|error| format!("failed to start native throbber thread: {error}"))?;

        let thread_id = ready_receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("native throbber did not initialize: {error}"))??;

        HANDLE
            .set(NativeThrobberHandle { sender, thread_id })
            .map_err(|_| "native throbber was initialized twice".to_string())
    }

    pub fn show_listening() -> Result<(), String> {
        send(Command::ShowListening)
    }

    pub fn hide() -> Result<(), String> {
        send(Command::Hide)
    }

    pub fn set_level(level: f32) -> Result<(), String> {
        send(Command::SetLevel(level.clamp(0.0, 1.0)))
    }

    pub fn show_confirmation(kind: &str, text: String) -> Result<(), String> {
        let kind = if kind == "duplicate" {
            ConfirmationKind::Duplicate
        } else {
            ConfirmationKind::Added
        };
        send(Command::ShowConfirmation { kind, text })
    }

    fn send(command: Command) -> Result<(), String> {
        let handle = HANDLE
            .get()
            .ok_or_else(|| "native throbber is unavailable".to_string())?;
        handle
            .sender
            .send(command)
            .map_err(|error| format!("native throbber thread stopped: {error}"))?;
        unsafe {
            PostThreadMessageW(handle.thread_id, WAKE_MESSAGE, WPARAM(0), LPARAM(0))
                .map_err(|error| format!("failed to wake native throbber: {error}"))?;
        }
        Ok(())
    }

    fn thread_main(receiver: Receiver<Command>, ready: mpsc::SyncSender<Result<u32, String>>) {
        let init = unsafe { OverlayState::new() };
        let mut state = match init {
            Ok(state) => {
                let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
                let _ = ready.send(Ok(thread_id));
                state
            }
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };

        unsafe {
            let mut message = MSG::default();
            loop {
                let result = GetMessageW(&mut message, None, 0, 0);
                if result.0 <= 0 {
                    break;
                }

                if message.message == WAKE_MESSAGE {
                    while let Ok(command) = receiver.try_recv() {
                        state.apply(command);
                    }
                } else if message.message == WM_TIMER
                    && message.hwnd == state.listening_hwnd
                    && message.wParam.0 == TIMER_ID
                {
                    state.tick();
                } else {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }

            drop(state);
            RoUninitialize();
        }
    }

    impl OverlayState {
        unsafe fn new() -> Result<Self, String> {
            RoInitialize(RO_INIT_SINGLETHREADED)
                .map_err(|error| format!("WinRT initialization failed: {error}"))?;

            let instance_module = GetModuleHandleW(None)
                .map_err(|error| format!("failed to get app module: {error}"))?;
            let instance = HINSTANCE(instance_module.0);

            let listening_class = windows::core::w!("AnnotateNativeThrobber");
            let confirmation_class = windows::core::w!("AnnotateNativeConfirmation");
            register_window_class(instance, listening_class)?;
            register_window_class(instance, confirmation_class)?;

            let listening_hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_TRANSPARENT,
                listening_class,
                windows::core::w!(""),
                WS_POPUP,
                0,
                0,
                LISTEN_WIDTH,
                OVERLAY_HEIGHT,
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(|error| format!("failed to create listening overlay: {error}"))?;

            let confirmation_hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_TRANSPARENT,
                confirmation_class,
                windows::core::w!(""),
                WS_POPUP,
                0,
                0,
                CONFIRM_CANVAS_WIDTH,
                OVERLAY_HEIGHT,
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(|error| format!("failed to create confirmation overlay: {error}"))?;

            let dpi = GetDpiForSystem().max(96);
            let scale = dpi as f32 / 96.0;
            let scene = create_composition_scene(listening_hwnd, scale)
                .map_err(|error| format!("Windows Composition setup failed: {error}"))?;

            Ok(Self {
                listening_hwnd,
                confirmation_hwnd,
                scene,
                scale,
                visible: false,
                level: 0.0,
                smoothed_level: 0.0,
                last_level_at: Instant::now(),
                animation_started_at: Instant::now(),
                animations_enabled: client_animations_enabled(),
            })
        }

        unsafe fn apply(&mut self, command: Command) {
            match command {
                Command::ShowListening => self.show_listening(),
                Command::Hide => self.hide(),
                Command::SetLevel(level) => {
                    self.level = level;
                    self.last_level_at = Instant::now();
                }
                Command::ShowConfirmation { kind, text } => self.show_confirmation(kind, &text),
            }
        }

        unsafe fn show_listening(&mut self) {
            let _ = ShowWindow(self.confirmation_hwnd, SW_HIDE);
            self.animations_enabled = client_animations_enabled();
            self.level = 0.0;
            self.smoothed_level = 0.0;
            self.last_level_at = Instant::now();
            self.animation_started_at = Instant::now();
            self.reset_dots();
            position_window(self.listening_hwnd, LISTEN_WIDTH, self.scale, true);
            let _ = ShowWindow(self.listening_hwnd, SW_SHOWNOACTIVATE);
            if self.animations_enabled {
                SetTimer(Some(self.listening_hwnd), TIMER_ID, 16, None);
            }
            self.visible = true;
        }

        unsafe fn show_confirmation(&mut self, kind: ConfirmationKind, text: &str) {
            self.hide();
            if let Err(error) = draw_confirmation(self.confirmation_hwnd, kind, text, self.scale) {
                log::error!("[NativeThrobber] Failed to draw confirmation: {error}");
                return;
            }
            let _ = ShowWindow(self.confirmation_hwnd, SW_SHOWNOACTIVATE);
        }

        unsafe fn hide(&mut self) {
            let _ = KillTimer(Some(self.listening_hwnd), TIMER_ID);
            let _ = ShowWindow(self.listening_hwnd, SW_HIDE);
            let _ = ShowWindow(self.confirmation_hwnd, SW_HIDE);
            self.level = 0.0;
            self.smoothed_level = 0.0;
            self.visible = false;
        }

        unsafe fn tick(&mut self) {
            if !self.visible || !self.animations_enabled {
                return;
            }

            if self.last_level_at.elapsed() > Duration::from_millis(250) {
                self.level = 0.0;
            }
            self.smoothed_level += (self.level - self.smoothed_level) * 0.22;

            let elapsed = self.animation_started_at.elapsed().as_secs_f32();
            let phase_offsets = [0.0_f32, 0.8, 1.6, 2.4];
            let energy = self.smoothed_level;
            for (index, dot) in self.scene.dots.iter().enumerate() {
                let wave = (elapsed * 3.5 + phase_offsets[index]).sin();
                let bounce = wave * (0.6 + energy * 2.0);
                let stretch = 1.0 + energy * 1.8;
                let dot_height = 6.0 * stretch * self.scale;
                let x = (15.0 + index as f32 * 12.0) * self.scale;
                let y = (13.0 + bounce) * self.scale - (dot_height - 6.0 * self.scale) / 2.0;
                let _ = dot.visual.SetSize(Vector2 {
                    X: 6.0 * self.scale,
                    Y: dot_height,
                });
                let _ = dot.visual.SetOffset(Vector3 { X: x, Y: y, Z: 1.0 });
                let radius = (3.0 * self.scale).min(dot_height / 2.0);
                let _ = dot.geometry.SetSize(Vector2 {
                    X: 6.0 * self.scale,
                    Y: dot_height,
                });
                let _ = dot.geometry.SetCornerRadius(Vector2 {
                    X: radius,
                    Y: radius,
                });
            }
        }

        unsafe fn reset_dots(&self) {
            for (index, dot) in self.scene.dots.iter().enumerate() {
                let _ = dot.visual.SetSize(Vector2 {
                    X: 6.0 * self.scale,
                    Y: 6.0 * self.scale,
                });
                let _ = dot.visual.SetOffset(Vector3 {
                    X: (15.0 + index as f32 * 12.0) * self.scale,
                    Y: 13.0 * self.scale,
                    Z: 1.0,
                });
                let radius = 3.0 * self.scale;
                let _ = dot.geometry.SetSize(Vector2 {
                    X: 6.0 * self.scale,
                    Y: 6.0 * self.scale,
                });
                let _ = dot.geometry.SetCornerRadius(Vector2 {
                    X: radius,
                    Y: radius,
                });
            }
        }
    }

    unsafe fn register_window_class(
        instance: HINSTANCE,
        class_name: windows::core::PCWSTR,
    ) -> Result<(), String> {
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(format!(
                "failed to register overlay window class: {}",
                windows::core::Error::from_win32()
            ));
        }
        Ok(())
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn create_composition_scene(hwnd: HWND, scale: f32) -> WindowsResult<CompositionScene> {
        let dispatcher = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;
        let compositor = Compositor::new()?;
        let desktop_interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = desktop_interop.CreateDesktopWindowTarget(hwnd, true)?;
        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        let pill = compositor.CreateSpriteVisual()?;
        pill.SetSize(Vector2 {
            X: 72.0 * scale,
            Y: 32.0 * scale,
        })?;
        pill.SetOffset(Vector3 {
            X: 0.0,
            Y: 0.0,
            Z: 0.0,
        })?;
        let pill_geometry = compositor.CreateRoundedRectangleGeometry()?;
        pill_geometry.SetSize(Vector2 {
            X: 72.0 * scale,
            Y: 32.0 * scale,
        })?;
        pill_geometry.SetCornerRadius(Vector2 {
            X: 16.0 * scale,
            Y: 16.0 * scale,
        })?;
        let pill_clip = compositor.CreateGeometricClipWithGeometry(&pill_geometry)?;
        pill.SetClip(&pill_clip)?;
        let pill_brush = compositor.CreateColorBrushWithColor(Color {
            A: 255,
            R: 18,
            G: 18,
            B: 18,
        })?;
        pill.SetBrush(&pill_brush)?;
        root.Children()?.InsertAtTop(&pill)?;

        let dot_brush = compositor.CreateColorBrushWithColor(Color {
            A: 217,
            R: 255,
            G: 255,
            B: 255,
        })?;
        let mut dots = Vec::with_capacity(4);
        for index in 0..4 {
            let dot = compositor.CreateSpriteVisual()?;
            dot.SetSize(Vector2 {
                X: 6.0 * scale,
                Y: 6.0 * scale,
            })?;
            dot.SetOffset(Vector3 {
                X: (15.0 + index as f32 * 12.0) * scale,
                Y: 13.0 * scale,
                Z: 1.0,
            })?;
            let dot_geometry = compositor.CreateRoundedRectangleGeometry()?;
            dot_geometry.SetSize(Vector2 {
                X: 6.0 * scale,
                Y: 6.0 * scale,
            })?;
            dot_geometry.SetCornerRadius(Vector2 {
                X: 3.0 * scale,
                Y: 3.0 * scale,
            })?;
            let dot_clip = compositor.CreateGeometricClipWithGeometry(&dot_geometry)?;
            dot.SetClip(&dot_clip)?;
            dot.SetBrush(&dot_brush)?;
            root.Children()?.InsertAtTop(&dot)?;
            dots.push(DotVisual {
                visual: dot,
                geometry: dot_geometry,
            });
        }

        Ok(CompositionScene {
            _dispatcher: dispatcher,
            _compositor: compositor,
            _target: target,
            _root: root,
            _pill: pill,
            _pill_geometry: pill_geometry,
            dots,
        })
    }

    unsafe fn position_window(hwnd: HWND, logical_width: i32, scale: f32, show: bool) {
        let width = (logical_width as f32 * scale).round() as i32;
        let height = (OVERLAY_HEIGHT as f32 * scale).round() as i32;
        let margin = (BOTTOM_MARGIN as f32 * scale).round() as i32;
        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let screen_height = GetSystemMetrics(SM_CYSCREEN);
        let x = (screen_width - width) / 2;
        let y = screen_height - height - margin;
        let flags = if show {
            SWP_NOACTIVATE | SWP_SHOWWINDOW
        } else {
            SWP_NOACTIVATE
        };
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, width, height, flags);
    }

    unsafe fn draw_confirmation(
        hwnd: HWND,
        kind: ConfirmationKind,
        text: &str,
        scale: f32,
    ) -> Result<(), String> {
        let width = (CONFIRM_CANVAS_WIDTH as f32 * scale).round() as i32;
        let height = (OVERLAY_HEIGHT as f32 * scale).round() as i32;
        let memory_dc = CreateCompatibleDC(None);
        if memory_dc.is_invalid() {
            return Err("failed to create confirmation drawing context".to_string());
        }

        let mut bitmap_info = BITMAPINFO::default();
        bitmap_info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            Some(memory_dc),
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .map_err(|error| format!("failed to create confirmation bitmap: {error}"))?;
        std::ptr::write_bytes(bits.cast::<u8>(), 0, (width * height * 4) as usize);
        let previous_bitmap = SelectObject(memory_dc, HGDIOBJ(bitmap.0));

        let font_height = -(12.0 * scale).round() as i32;
        let font = CreateFontW(
            font_height,
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            windows::core::w!("Segoe UI"),
        );
        let previous_font = SelectObject(memory_dc, HGDIOBJ(font.0));
        SetBkMode(memory_dc, TRANSPARENT);

        let mut wide_text: Vec<u16> = text.encode_utf16().collect();
        if wide_text.is_empty() {
            wide_text.push(' ' as u16);
        }
        let mut measured = SIZE::default();
        let _ = GetTextExtentPoint32W(memory_dc, &wide_text, &mut measured);

        let padding = (12.0 * scale).round() as i32;
        let icon_size = (14.0 * scale).round() as i32;
        let gap = (6.0 * scale).round() as i32;
        let min_pill = (72.0 * scale).round() as i32;
        let max_pill = (240.0 * scale).round() as i32;
        let pill_width = (padding * 2 + icon_size + gap + measured.cx).clamp(min_pill, max_pill);
        let pill_left = (width - pill_width) / 2;
        let pill_right = pill_left + pill_width;

        let pill_brush = CreateSolidBrush(rgb(18, 18, 18));
        let pill_pen = CreatePen(PS_SOLID, 1, rgb(18, 18, 18));
        let previous_brush = SelectObject(memory_dc, HGDIOBJ(pill_brush.0));
        let previous_pen = SelectObject(memory_dc, HGDIOBJ(pill_pen.0));
        let radius = height;
        let _ = RoundRect(memory_dc, pill_left, 0, pill_right, height, radius, radius);

        let icon_left = pill_left + padding;
        let icon_top = (height - icon_size) / 2;
        let icon_color = match kind {
            ConfirmationKind::Added => rgb(74, 222, 128),
            ConfirmationKind::Duplicate => rgb(251, 191, 36),
        };
        let icon_pen = CreatePen(PS_SOLID, (2.0 * scale).round().max(2.0) as i32, icon_color);
        SelectObject(memory_dc, HGDIOBJ(icon_pen.0));

        match kind {
            ConfirmationKind::Added => {
                let _ = MoveToEx(
                    memory_dc,
                    icon_left + (2.0 * scale).round() as i32,
                    icon_top + (7.0 * scale).round() as i32,
                    None,
                );
                let _ = LineTo(
                    memory_dc,
                    icon_left + (6.0 * scale).round() as i32,
                    icon_top + (11.0 * scale).round() as i32,
                );
                let _ = LineTo(
                    memory_dc,
                    icon_left + (12.0 * scale).round() as i32,
                    icon_top + (3.0 * scale).round() as i32,
                );
            }
            ConfirmationKind::Duplicate => {
                SelectObject(memory_dc, GetStockObject(NULL_BRUSH));
                let inset = (1.0 * scale).round() as i32;
                let _ = Ellipse(
                    memory_dc,
                    icon_left + inset,
                    icon_top + inset,
                    icon_left + icon_size - inset,
                    icon_top + icon_size - inset,
                );
                let _ = MoveToEx(
                    memory_dc,
                    icon_left + (4.0 * scale).round() as i32,
                    icon_top + icon_size / 2,
                    None,
                );
                let _ = LineTo(
                    memory_dc,
                    icon_left + (10.0 * scale).round() as i32,
                    icon_top + icon_size / 2,
                );
            }
        }

        SelectObject(memory_dc, HGDIOBJ(font.0));
        SetTextColor(memory_dc, rgb(255, 255, 255));
        let text_left = icon_left + icon_size + gap;
        let mut text_rect = RECT {
            left: text_left,
            top: 0,
            right: pill_right - padding,
            bottom: height,
        };
        let _ = DrawTextW(
            memory_dc,
            &mut wide_text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );

        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (width * height) as usize);
        for pixel in pixels {
            if *pixel & 0x00ff_ffff != 0 {
                *pixel |= 0xff00_0000;
            }
        }

        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let screen_height = GetSystemMetrics(SM_CYSCREEN);
        let margin = (BOTTOM_MARGIN as f32 * scale).round() as i32;
        let destination = POINT {
            x: (screen_width - width) / 2,
            y: screen_height - height - margin,
        };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let update_result = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&destination),
            Some(&size),
            Some(memory_dc),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        SelectObject(memory_dc, previous_pen);
        SelectObject(memory_dc, previous_brush);
        SelectObject(memory_dc, previous_font);
        SelectObject(memory_dc, previous_bitmap);
        let _ = DeleteObject(HGDIOBJ(icon_pen.0));
        let _ = DeleteObject(HGDIOBJ(pill_pen.0));
        let _ = DeleteObject(HGDIOBJ(pill_brush.0));
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);

        update_result.map_err(|error| format!("failed to update confirmation window: {error}"))
    }

    fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
        COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
    }

    unsafe fn client_animations_enabled() -> bool {
        use windows::core::BOOL;
        use windows::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };

        let mut enabled = BOOL(1);
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && enabled.as_bool()
    }
}

#[cfg(target_os = "windows")]
pub use imp::{hide, initialize, set_level, show_confirmation, show_listening};

#[cfg(not(target_os = "windows"))]
pub fn initialize() -> Result<(), String> {
    Ok(())
}
#[cfg(not(target_os = "windows"))]
pub fn show_listening() -> Result<(), String> {
    Ok(())
}
#[cfg(not(target_os = "windows"))]
pub fn hide() -> Result<(), String> {
    Ok(())
}
#[cfg(not(target_os = "windows"))]
pub fn set_level(_level: f32) -> Result<(), String> {
    Ok(())
}
#[cfg(not(target_os = "windows"))]
pub fn show_confirmation(_kind: &str, _text: String) -> Result<(), String> {
    Ok(())
}
