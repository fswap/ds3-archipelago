use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Error, Result};
use hudhook::{ImguiRenderLoop, RenderContext};
use imgui::*;
use log::*;

use crate::{Core, Game, InputBlocker, InputFlags, overlay::Overlay, utils::PopupModalExt};

/// The duration between debug prints of the frame timing data.
const TIME_PER_FRAME_PRINT: Duration = Duration::from_secs(5);

/// A wrapper around the rest of the mod's UI that doesn't expect any state to
/// exist. This allows the full [Overlay] to assume that its [Core] exists while
/// still using Hudhook and ImGui to surface fatal errors that may occur during
/// initialization.
pub(crate) struct ErrorDisplay<G: Game> {
    /// The struct that's used to block and unblock input going to the game.
    input_blocker: G::InputBlocker,

    /// The main overlay if it managed to initialize correctly, or [None]
    /// otherwise.
    overlay: Option<Overlay<G>>,

    /// The core game logic. Used to extract fatal errors to display to the
    /// user.
    core: Option<Arc<Mutex<G::Core>>>,

    /// A fatal error to display. Once set, this can't be changed, even if other
    /// fatal errors are detected later.
    error: Option<Error>,

    /// Whether to display the full error information or just the summary.
    show_full_error: bool,

    /// The time it took us to do mod-specific work over the most recent frames.
    /// This is cleared each time the average is printed.
    frame_times: Vec<Duration>,

    /// The time the last frame average was printed.
    last_frame_printed: Instant,
}

impl<G: Game> ErrorDisplay<G> {
    /// Creates a new [ErrorDisplay] that will only ever be run
    pub fn new(core: Result<Arc<Mutex<G::Core>>>, input_blocker: G::InputBlocker) -> Self {
        let frame_times = Vec::with_capacity(
            // Allocate enough space for 60fps.
            TIME_PER_FRAME_PRINT
                .as_millis()
                .div_ceil((Duration::from_secs(1) / 60).as_millis())
                .try_into()
                .unwrap(),
        );
        match core {
            Ok(core) => Self {
                input_blocker,
                overlay: Some(Overlay::new()),
                core: Some(core),
                error: None,
                show_full_error: false,
                frame_times,
                last_frame_printed: Instant::now(),
            },
            Err(error) => Self {
                input_blocker,
                overlay: None,
                core: None,
                error: Some(error),
                show_full_error: false,
                frame_times,
                last_frame_printed: Instant::now(),
            },
        }
    }

    /// Displays a fatal error to the user if one is set.
    fn render_error(&mut self, ui: &mut Ui) {
        let Some(error) = &self.error else { return };

        // Make sure the cursor is visible even if the player is loaded into a
        // save with the menu closed.
        //
        // Safety: This is only ever run on the main thread.
        unsafe {
            G::force_cursor_visible();
        }

        unsafe {
            imgui_sys::igSetNextWindowSize(
                [800., if self.show_full_error { 500. } else { 0. }].into(),
                Condition::Always as i32,
            );
        }

        ui.open_popup("#fatal-error");
        ui.modal_popup_config("#fatal-error")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .size(
                [800., if self.show_full_error { 500. } else { 0. }],
                Condition::Always,
            )
            .build(|| {
                ui.checkbox("Show full error", &mut self.show_full_error);
                ui.text_wrapped(if self.show_full_error {
                    format!("{:?}", error)
                } else {
                    error.to_string()
                });

                ui.separator();
                if ui.button("Exit") {
                    std::process::exit(1);
                }
            });
    }
}

impl<G: Game> ImguiRenderLoop for ErrorDisplay<G> {
    fn render(&mut self, ui: &mut Ui) {
        let start = Instant::now();
        let io = ui.io();
        let mut flag = InputFlags::empty();
        if io.want_capture_mouse {
            flag |= InputFlags::Mouse;
        }
        if io.want_capture_keyboard {
            flag |= InputFlags::Keyboard;
        }
        if io.want_capture_mouse && io.want_capture_keyboard {
            // Only block pad input if both the mouse and keyboard are blocked
            // (for example if a modal dialog is up).
            flag |= InputFlags::GamePad;
        }
        self.input_blocker.block_only(flag);

        if let Some(core) = &mut self.core {
            let mut core = core.lock().unwrap();
            if let Some(overlay) = &mut self.overlay {
                overlay.render(ui, &mut core);
            }

            if self.error.is_none() {
                self.error = core.base_mut().take_error();
            }
        }

        self.render_error(ui);

        let now = Instant::now();
        self.frame_times.push(now.duration_since(start));
        if now.duration_since(self.last_frame_printed) >= TIME_PER_FRAME_PRINT {
            let frames = u32::try_from(self.frame_times.len()).unwrap();
            let fps = f64::from(frames) / TIME_PER_FRAME_PRINT.as_secs_f64();
            let ap_time_per_frame = self.frame_times.iter().copied().sum::<Duration>() / frames;
            let total_time_per_frame = TIME_PER_FRAME_PRINT / frames;
            info!(
                "In last {TIME_PER_FRAME_PRINT:?}: {frames} frames rendered ({:02} FPS), AP took \
                 {:?}/frame avg ({:.2}% of frame)",
                fps,
                ap_time_per_frame,
                100.0 * (ap_time_per_frame.as_micros() as f64)
                    / (total_time_per_frame.as_micros() as f64),
            );
            self.frame_times.clear();
            self.last_frame_printed = now;
        }
    }

    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        ctx.set_clipboard_backend(crate::clipboard::WindowsClipboardBackend {});
    }

    fn before_render<'a>(
        &'a mut self,
        ctx: &mut Context,
        render_context: &'a mut dyn RenderContext,
    ) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.before_render(ctx, render_context);
        } else {
            // Set the font scale here to match the overlay's logic.
            ctx.io_mut().font_global_scale = 1.8;
        }
    }
}
