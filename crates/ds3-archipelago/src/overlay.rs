use std::{mem, ptr, str::FromStr};

use archipelago_rs::{self as ap, RichText, TextColor};
use darksouls3::sprj::{EventFlag, MapItemMan, MenuMan, SprjEventFlagMan};
use fromsoftware_shared::FromStatic;
use hudhook::RenderContext;
use imgui::*;
use imgui_sys::igSetWindowFocus_Str;
use log::*;
use regex_macro::regex;

use crate::core::Core;

mod text_input_history;

use text_input_history::TextInputHistory;

const GREEN: ImColor32 = ImColor32::from_rgb(0x8A, 0xE2, 0x43);
const RED: ImColor32 = ImColor32::from_rgb(0xFF, 0x44, 0x44);
const WHITE: ImColor32 = ImColor32::from_rgb(0xFF, 0xFF, 0xFF);
// This is the darkest gray that still meets WCAG guidelines for contrast with
// the black background of the overlay.
const BLACK: ImColor32 = ImColor32::from_rgb(0x9C, 0x9C, 0x9C);
const YELLOW: ImColor32 = ImColor32::from_rgb(0xFC, 0xE9, 0x4F);
const BLUE: ImColor32 = ImColor32::from_rgb(0x82, 0xA9, 0xD4);
const MAGENTA: ImColor32 = ImColor32::from_rgb(0xBF, 0x9B, 0xBC);
const CYAN: ImColor32 = ImColor32::from_rgb(0x34, 0xE2, 0xE2);

/// The visual overlay that appears on top of the game.
#[derive(Default)]
pub struct Overlay {
    /// The last-known size of the viewport. This is only set once hudhook has
    /// been initialized and the viewport has a non-zero size.
    viewport_size: Option<[f32; 2]>,

    /// The URL field in the modal connection popup.
    popup_url: String,

    /// The text the user typed in the say input.
    say_input: String,

    /// The history of messages sent to the say input.
    say_history: TextInputHistory,

    /// Whether the log was previously scrolled all the way down.
    log_was_scrolled_down: bool,

    /// The number of logs that were most recently emitted. This is used to
    /// determine when new logs are emitted for [frames_since_new_logs].
    logs_emitted: usize,

    /// The number of frames that have elapsed since new logs were last added.
    /// We use this to determine when to auto-scroll the log window.
    frames_since_new_logs: u64,

    /// The current font scale for the overlay UI.
    font_scale: f32,

    /// The unfocused window opacity for the overlay UI.
    unfocused_window_opacity: f32,

    /// Whether the settings window is currently visible.
    settings_window_visible: bool,

    /// Whether the game was on the main menu in the previous frame.
    was_main_menu: bool,

    /// Whether the overlay window was focused in the previous frame.
    was_window_focused: bool,

    /// Whether compact mode was enabled in the previous frame.
    was_compact_mode: bool,

    /// Whether to focus the say input on the next frame. Used to keep focus
    /// after the user pressed enter.
    focus_say_input_next_frame: bool,

    /// The size of the main overlay window in the previous frame. Used to
    /// resize when entering and exiting compact mode.
    previous_size: Option<[f32; 2]>,
}

// Safety: The sole Overlay instance is owned by Hudhook, which only ever
// interacts with it during frame rendering. We know DS3 frame rendering always
// happens on the main thread, and never in parallel, so synchronization is not
// a real concern.
unsafe impl Sync for Overlay {}

impl Overlay {
    /// Creates a new instance of the overlay and the core mod logic.
    pub fn new() -> Self {
        Self {
            font_scale: 1.8,
            unfocused_window_opacity: 0.4,
            was_compact_mode: true,
            ..Default::default()
        }
    }

    /// Like [ImguiRenderLoop::render], but takes a reference to [Core] as well.
    ///
    /// We don't store `core` directly in the overlay so that we can ensure that
    /// its mutex is only locked once per render.
    pub fn render(&mut self, ui: &mut Ui, core: &mut Core) {
        self.render_main_window(ui, core);
        self.render_settings_window(ui);
    }

    /// See [ImguiRenderLoop::before_render], but takes a reference to [Core] as
    /// well.
    pub fn before_render<'a>(
        &'a mut self,
        ctx: &mut Context,
        _render_context: &'a mut dyn RenderContext,
    ) {
        self.frames_since_new_logs += 1;
        self.viewport_size = match ctx.main_viewport().size {
            [0., 0.] => None,
            size => Some(size),
        };

        // Set the font scale here because we need the frame height later to
        // calculate the main window size, which depends on it.
        ctx.io_mut().font_global_scale = self.font_scale;
    }

    /// Render the primary overlay window and any popups it opens.
    fn render_main_window(&mut self, ui: &Ui, core: &mut Core) {
        let Some(viewport_size) = self.viewport_size else {
            return;
        };

        // By default, imgui doesn't remove focus when escape is pressed, even
        // though it does relinquish its claim to the mouse and keyboard.
        // Because we use focus to determine when to make the overlay
        // transparent, we want it to be removed more aggressivley, so we do so
        // manually.
        if ui.is_key_pressed(Key::Escape) ||
            // Also defocus the window any time the player loads into the game.
            // This ensures that controller players don't have to mess with the
            // keyboard and mouse just to get the overlay unfocused.
            (self.was_main_menu && !self.is_main_menu())
        {
            unsafe { igSetWindowFocus_Str(ptr::null()) };
        }

        let window_opacity = if self.was_window_focused {
            1.0
        } else {
            self.unfocused_window_opacity
        };
        let mut bg_color = [0.0, 0.0, 0.0, window_opacity];
        let _bg = ui.push_style_color(StyleColor::WindowBg, bg_color);
        let _menu_bg = ui.push_style_color(StyleColor::MenuBarBg, bg_color);
        bg_color[3] = 1.0; // Popup backgrounds should always be fully opaque.
        let _popup_bg = ui.push_style_color(StyleColor::PopupBg, bg_color);

        let mut builder = ui
            .window(format!(
                "Archipelago Client {} [{}]###ap-client-overlay",
                env!("CARGO_PKG_VERSION"),
                match core.connection_state_type() {
                    ap::ConnectionStateType::Connected => "Connected",
                    ap::ConnectionStateType::Connecting => "Connecting...",
                    ap::ConnectionStateType::Disconnected => "Disconnected",
                }
            ))
            .position([viewport_size[0] - 30., 30.], Condition::FirstUseEver)
            .position_pivot([1., 0.])
            .menu_bar(true);

        // When the menu opens or closes, add or remove space from the bottom of
        // the overlay for the message bar and horizontal scrollbar.
        let is_compact_mode = self.is_compact_mode(core);
        builder = match (self.previous_size, is_compact_mode, self.was_compact_mode) {
            (Some(size), true, false) => {
                let style = ui.clone_style();
                let remove_bottom_space =
                    ui.frame_height() + style.window_padding[1] + style.scrollbar_size;

                builder.size(
                    [size[0], size[1] - remove_bottom_space.ceil()],
                    Condition::Always,
                )
            }
            (Some(size), false, true) => {
                let style = ui.clone_style();
                let add_bottom_space =
                    ui.frame_height() + style.window_padding[1] + style.scrollbar_size;

                builder.size(
                    [size[0], size[1] + add_bottom_space.ceil()],
                    Condition::Always,
                )
            }
            _ => builder.size([viewport_size[0] * 0.4, 300.], Condition::FirstUseEver),
        };

        let focus_say_input = mem::take(&mut self.focus_say_input_next_frame);
        let collapsed = builder
            .build(|| {
                self.render_menu_bar(ui);
                ui.separator();
                self.render_log_window(ui, core);
                if !is_compact_mode {
                    if core.is_disconnected() {
                        self.render_connection_buttons(ui, core);
                    } else {
                        self.render_say_input(ui, core, focus_say_input);
                    }
                }
                self.render_url_modal_popup(ui, core);

                self.was_window_focused =
                    ui.is_window_focused_with_flags(WindowFocusedFlags::ROOT_AND_CHILD_WINDOWS);
                self.previous_size = Some(ui.window_size());
            })
            .is_none();

        self.was_main_menu = self.is_main_menu();
        self.was_compact_mode = is_compact_mode;

        if collapsed {
            self.was_window_focused = false;
        }
    }

    /// Renders the modal popup which queries the player for connection
    /// information.
    fn render_url_modal_popup(&mut self, ui: &Ui, core: &mut Core) {
        ui.modal_popup_config("#url-modal-popup")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .always_auto_resize(true)
            .build(|| {
                {
                    let _item_width = ui.push_item_width(500. * self.font_scale);
                    ui.input_text("Room URL", &mut self.popup_url)
                        .hint("archipelago.gg:12345")
                        .chars_noblank(true)
                        .build();
                }

                ui.disabled(self.popup_url.is_empty(), || {
                    if ui.button("Connect") {
                        ui.close_current_popup();
                        if let Err(e) = core.update_url(&self.popup_url) {
                            error!("Failed to save config: {e}");
                        }
                    }
                });
            });
    }

    /// Renders the menu bar.
    fn render_menu_bar(&mut self, ui: &Ui) {
        ui.menu_bar(|| {
            if ui.menu_item("Settings") {
                log::warn!("Click registered");
                self.settings_window_visible = true;
            }
        });
    }

    /// Renders the settings popup.
    fn render_settings_window(&mut self, ui: &Ui) {
        if !self.settings_window_visible {
            return;
        }

        let settings_bg_color = [0.0, 0.0, 0.0, 1.0];
        let _bg = ui.push_style_color(StyleColor::WindowBg, settings_bg_color);

        ui.window("Archipelago Overlay Settings")
            .size([0., 0.], Condition::Appearing)
            .position_pivot([0.5, 0.5])
            .collapsible(false)
            .build(|| {
                ui.text("Font Size ");
                ui.same_line();
                if ui.button("-##font-size-decrease-button") {
                    self.font_scale = (self.font_scale - 0.1).max(0.5);
                }
                ui.same_line();
                if ui.button("+##font-size-increase-button") {
                    self.font_scale = (self.font_scale + 0.1).min(4.0);
                }

                let mut opacity_percent = (self.unfocused_window_opacity * 100.0).round() as i32;
                let _slider_width = ui.push_item_width(150. * self.font_scale);
                ui.text("Unfocused Opacity ");
                ui.same_line();
                ui.slider_config("##unfocused-opacity-slider", 0, 100)
                    .display_format("%d%%")
                    .build(&mut opacity_percent);
                self.unfocused_window_opacity = (opacity_percent as f32) / 100.0;

                if ui.button("Ok") {
                    self.settings_window_visible = false;
                }
            });
    }

    /// Renders the buttons that allow the player to reconnect to Archipelago.
    /// These take the place of the text box when the client is disconnected.
    fn render_connection_buttons(&mut self, ui: &Ui, core: &mut Core) {
        if ui.button("Reconnect") {
            core.reconnect();
        }

        ui.same_line();
        if ui.button("Change URL") {
            ui.open_popup("#url-modal-popup");
            core.config().url().clone_into(&mut self.popup_url);
        }
    }

    /// Renders the log window which displays all the prints sent from the server.
    fn render_log_window(&mut self, ui: &Ui, core: &Core) {
        let style = ui.clone_style();

        let scrollbar_bg_opacity = if self.was_window_focused { 1.0 } else { 0.0 };
        let scrollbar_bg_color = [0.0, 0.0, 0.0, scrollbar_bg_opacity];
        let _scrollbar_bg = ui.push_style_color(StyleColor::ScrollbarBg, scrollbar_bg_color);

        let _item_spacing = ui.push_style_var(StyleVar::ItemSpacing([
            style.item_spacing[0],
            style.window_padding[1],
        ]));

        let is_compact_mode = self.is_compact_mode(core);
        let input_height = if !is_compact_mode {
            ui.frame_height_with_spacing()
        } else {
            0.0
        };

        ui.child_window("#log")
            .size([0.0, -input_height.ceil()])
            .draw_background(false)
            .always_vertical_scrollbar(true)
            .always_horizontal_scrollbar(!is_compact_mode)
            .build(|| {
                let logs = core.logs();
                if logs.len() != self.logs_emitted {
                    self.frames_since_new_logs = 0;
                    self.logs_emitted = logs.len();
                }

                for message in logs {
                    use ap::Print::*;
                    write_message_data(
                        ui,
                        message.data(),
                        // De-emphasize miscellaneous server prints.
                        match message {
                            Chat { .. }
                            | ServerChat { .. }
                            | Tutorial { .. }
                            | CommandResult { .. }
                            | AdminCommandResult { .. }
                            | Unknown { .. } => 0xff,
                            ItemSend { item, .. } | ItemCheat { item, .. } | Hint { item, .. }
                                if core.config().slot() == item.receiver().name()
                                    || core.config().slot() == item.sender().name() =>
                            {
                                0xFF
                            }
                            _ => 0xAA,
                        },
                    );
                }
                if self.log_was_scrolled_down && self.frames_since_new_logs < 10 {
                    ui.set_scroll_y(ui.scroll_max_y());
                }
                self.log_was_scrolled_down = ui.scroll_y() == ui.scroll_max_y();
            });
    }

    /// Renders the text box in which users can write chats to the server.
    ///
    /// If `focus` is true, this forces the input to be in focus.
    fn render_say_input(&mut self, ui: &Ui, core: &mut Core, focus: bool) {
        ui.disabled(core.client().is_none(), || {
            let arrow_button_width = ui.frame_height(); // Arrow buttons are square buttons.
            let style = ui.clone_style();
            let spacing = style.item_spacing[0] * self.font_scale * 0.7;

            let input_width = ui.push_item_width(-(arrow_button_width + spacing));
            if focus {
                ui.set_keyboard_focus_here();
            }
            let mut send = ui
                .input_text("##say-input", &mut self.say_input)
                .enter_returns_true(true)
                .callback(InputTextCallback::HISTORY, &mut self.say_history)
                .build();
            drop(input_width);

            ui.same_line_with_spacing(0.0, spacing);
            send = ui.arrow_button("##say-button", Direction::Right) || send;

            if send {
                // We don't have a great way to surface these errors, and
                // they're non-fatal, so just ignore them.
                let line = mem::take(&mut self.say_input);
                self.say_history.add(line.clone());
                self.say(line, core);
                self.focus_say_input_next_frame = true;
            }
        });
    }

    /// Handles a command from the player, falling back to sending it to the
    /// server.
    fn say(&mut self, message: String, core: &mut Core) {
        let Some(captures) = regex!("^(![^ ]+)( +)?(.*)?$").captures(message.trim()) else {
            let _ = core.client_mut().unwrap().say(message);
            return;
        };

        let command = captures.get(1).unwrap().as_str();

        let arg = || -> Option<&str> { Some(captures.get(3)?.as_str()) };

        let mut arg_error = |usage: &str| {
            core.log(vec![
                RichText::Color {
                    text: format!("Invalid {}.", command),
                    color: ap::TextColor::Red,
                },
                " Usage:\n".into(),
                usage.into(),
            ]);
        };

        match command {
            "!getevent" => {
                let Some(flag) = arg().and_then(|f| u32::from_str(f).ok()) else {
                    arg_error("!getevent EVENT_FLAG");
                    return;
                };

                let Ok(flag) = EventFlag::try_from(flag) else {
                    core.log(RichText::Color {
                        text: format!("Invalid event ID: {}", flag),
                        color: ap::TextColor::Red,
                    });
                    return;
                };

                let Ok(events) = (unsafe { SprjEventFlagMan::instance() }) else {
                    core.log(RichText::Color {
                        text: "SprjEventFlagMan not loaded".into(),
                        color: ap::TextColor::Red,
                    });
                    return;
                };

                let value = events.get_flag(flag);
                core.log(vec![
                    "Event ".into(),
                    RichText::Color {
                        // TODO: Use `u32::from()` once EventFlag supports it
                        text: format!("{:?}", unsafe { mem::transmute::<EventFlag, u32>(flag) }),
                        color: ap::TextColor::Blue,
                    },
                    ": ".into(),
                    RichText::Color {
                        text: format!("{:?}", value),
                        color: if value {
                            ap::TextColor::Green
                        } else {
                            ap::TextColor::Red
                        },
                    },
                ]);
            }

            #[cfg(debug_assertions)]
            "!setevent" => {
                let Some((flag, value)) = arg().and_then(|a| {
                    let args = regex!(" +").split(a).collect::<Vec<_>>();
                    if args.len() == 2 {
                        Some((u32::from_str(args[0]).ok()?, bool::from_str(args[1]).ok()?))
                    } else {
                        None
                    }
                }) else {
                    arg_error("!setevent EVENT_FLAG BOOL");
                    return;
                };

                let Ok(flag) = EventFlag::try_from(flag) else {
                    core.log(RichText::Color {
                        text: format!("Invalid event ID: {}", flag),
                        color: ap::TextColor::Red,
                    });
                    return;
                };

                let Ok(events) = (unsafe { SprjEventFlagMan::instance() }) else {
                    core.log(RichText::Color {
                        text: "SprjEventFlagMan not loaded".into(),
                        color: ap::TextColor::Red,
                    });
                    return;
                };

                events.set_flag(flag, value);
                core.log(vec![
                    "Set event ".into(),
                    RichText::Color {
                        // TODO: Use `u32::from()` once EventFlag supports it
                        text: format!("{:?}", unsafe { mem::transmute::<EventFlag, u32>(flag) }),
                        color: ap::TextColor::Blue,
                    },
                    " to ".into(),
                    RichText::Color {
                        text: format!("{:?}", value),
                        color: if value {
                            ap::TextColor::Green
                        } else {
                            ap::TextColor::Red
                        },
                    },
                ]);
            }

            _ => {
                let _ = core.client_mut().unwrap().say(message);
            }
        }
    }

    /// Returns whether the overlay is currently in "compact mode", where the
    /// bottommost widgets are not rendered.
    fn is_compact_mode(&self, core: &Core) -> bool {
        if core.is_disconnected() {
            // When the connection is inactive, always show the buttons to
            // reconnect.
            false
        } else if let Ok(menu_man) = unsafe { MenuMan::instance() } {
            !menu_man.is_menu_mode() && !self.is_main_menu()
        } else {
            true
        }
    }

    /// Returns whether the game is currently on the main menu.
    fn is_main_menu(&self) -> bool {
        // If MapItemMan isn't available, that usually means we're on the
        // main menu. There's probably a better way to detect that but we
        // don't know it yet.
        unsafe { MapItemMan::instance() }.is_err()
    }
}

trait ImColor32Ext {
    /// Returns a copy of [self] with its opacity overridden by [alpha].
    fn with_alpha(&self, alpha: u8) -> ImColor32;
}

impl ImColor32Ext for ImColor32 {
    fn with_alpha(&self, alpha: u8) -> ImColor32 {
        ImColor32::from_bits((self.to_bits() & 0x00ffffff) | ((alpha as u32) << 24))
    }
}

/// Writes the text in [parts] to [ui] in a single line.
fn write_message_data(ui: &Ui, parts: &[RichText], alpha: u8) {
    let mut first = true;
    for part in parts {
        if !first {
            ui.same_line();
        }
        first = false;

        // TODO: Load in fonts to support bold, maybe write a line manually for
        // underline? I'm not sure there's a reasonable way to support
        // background colors.
        use RichText::*;
        use TextColor::*;
        let color = match part {
            Player { .. } | PlayerName { .. } | Color { color: Blue, .. } => BLUE,
            Item { .. } | Color { color: Magenta, .. } => MAGENTA,
            Location { .. } | EntranceName { .. } | Color { color: Cyan, .. } => CYAN,
            Color { color: Black, .. } => BLACK,
            Color { color: Red, .. } => RED,
            Color { color: Green, .. } => GREEN,
            Color { color: Yellow, .. } => YELLOW,
            _ => WHITE,
        };
        ui.text_colored(color.with_alpha(alpha).to_rgba_f32s(), part.to_string());
    }
}
