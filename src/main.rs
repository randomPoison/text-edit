extern crate app_units;
extern crate clap;
extern crate glutin;
extern crate gleam;
extern crate rusttype;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate webrender;
extern crate webrender_traits;

use app_units::Au;
use clap::*;
use gleam::gl;
use glutin::*;
use webrender_traits::*;
use rusttype::*;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// The font size in pixels (measuring the vertical height of the font).
const FONT_SIZE_PX: f32 = 15.0;

/// The height of a line as a multiple of font size.
const LINE_HEIGHT: f32 = 1.5;

/// Magic constant current being used to handle font scaling.
///
/// See https://github.com/excaliburHisSheath/text-edit/issues/4 for more info.
const PIXEL_TO_POINT: f32 = 0.75;

/// Enables debug rendering of glyph bounding boxes.
const DEBUG_GLYPHS: bool = false;

fn main() {
    let matches = App::new("text-edit")
        .version("0.1")
        .author("David LeGare <excaliburhissheath@gmail.com>")
        .about("Edits text poorly")
        .arg(Arg::with_name("file").required(false))
        .get_matches();

    let initial_file = matches.value_of("file");

    // Load sample font into memory for layout purposes.
    let mut file = File::open("res/Hack-Regular.ttf").unwrap();
    let mut font_bytes = vec![];
    file.read_to_end(&mut font_bytes).unwrap();

    let font = FontCollection::from_bytes(&*font_bytes).into_font().unwrap();

    // Create a new glutin window and make its OpenGL context active.
    // ============================================================================================
    let window = WindowBuilder::new()
                .with_title("WebRender Sample")
                .with_gl(GlRequest::Specific(Api::OpenGl, (3, 2)))
                .build()
                .unwrap();

    unsafe {
        window.make_current().ok();
        gl::load_with(|symbol| window.get_proc_address(symbol) as *const _);
    }

    println!("OpenGL version {}", gl::get_string(gl::VERSION));

    // Configure and build the webrender instance.
    // ============================================================================================
    let opts = webrender::RendererOptions {
        device_pixel_ratio: window.hidpi_factor(),
        // debug: true,
        precache_shaders: true,
        enable_scrollbars: true,
        .. Default::default()
    };

    // Create the renderer and its associated `RenderApi` object.
    let (mut renderer, sender) = webrender::renderer::Renderer::new(opts);
    let api = sender.create_api();

    // Create a `Notifier` object to notify the window when a frame is ready.
    let notifier = Box::new(Notifier::new(window.create_window_proxy()));
    renderer.set_render_notifier(notifier);

    let epoch = Epoch(0);
    let root_background_color = ColorF::new(0.1, 0.1, 0.1, 1.0);

    // Set the root pipeline, I don't know what this is for, but it's necessary currently.
    let pipeline_id = PipelineId(0, 0);
    api.set_root_pipeline(pipeline_id);

    let font_key = api.add_raw_font(font_bytes.clone());

    let hidpi_factor = window.hidpi_factor();

    // Launch and connect to xi-core.
    // ============================================================================================

    // TODO: This currently requires that xi-core be in the system PATH
    let xi_process = Command::new("xi-core")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Somehow failed to run xi-core command, maybe it's not installed?");

    // Get input and output pipes for xi-core.
    let mut xi_stdin = xi_process.stdin.expect("No stdin pipe to xi-core child process");
    let xi_stdout = xi_process.stdout.expect("No stdout pipe to xi-core child process");
    let xi_stdout = BufReader::new(xi_stdout);

    // Open a tab.
    // TODO: Actually track the name of the new tab and use it in future messages.
    writeln!(xi_stdin, "{}", r#"{"id":0,"method":"new_tab","params":[]}"#).expect("Failed to send message to xi-core");

    // Change the visible region of the file (no response).
    let (window_width, window_height) = window.get_inner_size().unwrap();
    let window_height_in_lines = window_height as f32 / (FONT_SIZE_PX * LINE_HEIGHT);
    writeln!(xi_stdin, r#"{{"method":"edit","params":{{"method":"scroll","params":[0, {}],"tab":"0"}}}}"#, window_height_in_lines as usize).expect("Failed to send message to xi-core");

    // Open this file and get the lines from the file.
    if let Some(file_path) = initial_file {
        writeln!(xi_stdin, r#"{{"method":"edit","params":{{"method":"open","params":{{"filename":"{}"}},"tab":"0"}}}}"#, file_path).expect("Failed to send message to xi-core");
    }

    // Spawn a thread to pull the responses form xi-core.
    let (sender, receiver) = mpsc::channel();
    let window_proxy = window.create_window_proxy();
    thread::spawn(move || {
        for result in xi_stdout.lines() {
            // Read response from creating the tab.
            let response = result.expect("Error receiving response from xi-core");
            sender.send(response).expect("Failed to send message to main thread");
            window_proxy.wakeup_event_loop();
        }
    });

    let mut editor = EditorState {
        height_in_lines: 0,
        first_line: 0,
        lines: Vec::new(),
        view_width_pixels: window_width as usize,
        view_height_pixels: window_height as usize,
        scroll_offset_pixels: 0.0,
    };

    // Generate initial frame.
    let builder = build_display_lists(
        pipeline_id,
        font_key,
        &font,
        &mut editor,
        None,
    );
    api.set_root_display_list(
        Some(root_background_color),
        epoch,
        LayoutSize::new(editor.view_width_pixels as f32, editor.view_height_pixels as f32),
        builder,
    );
    api.generate_frame();

    // Main event loop.
    // =============================================================================================
    let mut dirty = false;
    for event in window.wait_events() {
        match event {
            Event::Closed => return,
            Event::KeyboardInput(element_state, _scan_code, virtual_key_code) => {
                if element_state == ElementState::Pressed {
                    if let Some(virtual_key_code) = virtual_key_code {
                        let message = match virtual_key_code {
                            VirtualKeyCode::Return => Some(r#"{"method":"edit","params":{"method":"insert_newline","params":{},"tab":"0"}}"#),
                            VirtualKeyCode::Back => Some(r#"{"method":"edit","params":{"method":"delete_backward","params":{},"tab":"0"}}"#),
                            VirtualKeyCode::Delete => Some(r#"{"method":"edit","params":{"method":"delete_forward","params":{},"tab":"0"}}"#),
                            VirtualKeyCode::Left => Some(r#"{"method":"edit","params":{"method":"move_left","params":{},"tab":"0"}}"#),
                            VirtualKeyCode::Right => Some(r#"{"method":"edit","params":{"method":"move_right","params":{},"tab":"0"}}"#),
                            VirtualKeyCode::Up => Some(r#"{"method":"edit","params":{"method":"move_up","params":{},"tab":"0"}}"#),
                            VirtualKeyCode::Down => Some(r#"{"method":"edit","params":{"method":"move_down","params":{},"tab":"0"}}"#),
                            _ => None,
                        };

                        if let Some(message) = message {
                            writeln!(xi_stdin, "{}", message).expect("Failed to send message to xi-core");
                        }
                    }
                }
            }
            Event::Resized(new_width, new_height) => {
                editor.view_width_pixels = new_width as usize;
                editor.view_height_pixels = new_height as usize;
                dirty = true;
            }
            Event::ReceivedCharacter(character) => {
                // TODO: OS X will send "private usage codepoints" which we want to filter out.
                // Issue tracker: https://github.com/excaliburHisSheath/text-edit/issues/2
                if !character.is_control() && !(character >= '\u{e000}' && character <= '\u{f8ff}') {
                    // Send the character to xi-core.
                    let message = format!(r#"{{"method":"edit","params":{{"method":"insert","params":{{"chars":"{}"}},"tab":"0"}}}}"#, character);
                    writeln!(xi_stdin, "{}", message).expect("Failed to send message to xi-core");
                }
            }
            _ => {},
        }

        let mut scroll_to_line = None;

        // Receive messages from xi-core.
        for message in receiver.try_iter() {
            // Parse the response string to structured JSON data.
            let update_value = serde_json::from_str::<Value>(&*message).expect("Failed to parse response json");

            // Look for "update" messages.
            // TODO: Look for all the other messages xi-core sends.
            if let Some(line_data) = update_value.search("lines") {
                editor.lines.clear();
                for line_contents in line_data.as_array().expect("\"lines\" wasn't an array") {
                    let line_contents = line_contents.as_array().expect("Line wasn't an array");

                    // TODO: If we're doing visible whitespace we don't want to trim the trailing
                    // whitespace.
                    // TODO: We probably want to perform unicode normalization here? Or maybe
                    // we want to do it when we generate the glyphs?
                    let line_string = line_contents[0]
                        .as_str()
                        .expect("First element of line wasn't a string")
                        .to_string();
                    let mut line_stuffffff = LineContents {
                        text: line_string,
                        cursors: Vec::new(),
                        selections: Vec::new(),
                    };

                    for line_control in &line_contents[1..] {
                        let line_control = line_control.as_array().expect("Line control wasn't an array");
                        let control_type = line_control[0].as_str().expect("First element of line control was not a string");
                        match control_type {
                            "cursor" => {
                                let col = line_control[1].as_u64().expect("Cursor index wasn't an integer");

                                // Xi internally represents cursor position as a `usize` so this cast
                                // shouldn't overflow.
                                line_stuffffff.cursors.push(col as usize);
                            }
                            "sel" => {
                                let start = line_control[1].as_u64().expect("Selection start wasn't an integer");
                                let end = line_control[2].as_u64().expect("Selection end wasn't an integer");

                                // Xi internally represents cursor position as a `usize` so these
                                // casts shouldn't overflow.
                                line_stuffffff.selections.push((start as usize, end as usize));
                            }
                            "fg" => { unimplemented!() }
                            _ => panic!("Unknown control type: {:?}", control_type),
                        }
                    }
                    editor.lines.push(line_stuffffff);
                }
            }

            if let Some(first_line) = update_value.search("first_line") {
                editor.first_line = first_line.as_u64().expect("\"first_line\" wasn't a number") as usize;
            }

            // Look for "scrollto" in the message.
            if let Some(scrollto) = update_value.search("scrollto") {
                let scrollto = scrollto.as_array().expect("\"scrollto\" was not an array");
                let line = scrollto[0].as_u64().expect("\"scrollto\" element wasn't an integer");
                scroll_to_line = Some(line as usize);
            }

            dirty = true;
        }

        if dirty {
            dirty = false;

            let builder = build_display_lists(
                pipeline_id,
                font_key,
                &font,
                &mut editor,
                scroll_to_line,
            );
            api.set_root_display_list(
                Some(root_background_color),
                epoch,
                LayoutSize::new(editor.view_width_pixels as f32, editor.view_height_pixels as f32),
                builder,
            );
            api.generate_frame();
        }

        renderer.update();
        renderer.render(DeviceUintSize::new(editor.view_width_pixels as u32, editor.view_height_pixels as u32) * hidpi_factor as u32);

        window.swap_buffers().ok();
    }
}

fn build_display_lists(
    pipeline_id: PipelineId,
    font_key: FontKey,
    font: &Font,
    editor: &mut EditorState,
    scroll_to_line: Option<usize>,
) -> DisplayListBuilder {
    let view_width = editor.view_width_pixels as f32;
    let view_height = editor.view_height_pixels as f32;

    let mut builder = DisplayListBuilder::new(pipeline_id);

    let bounds = LayoutRect::new(LayoutPoint::new(0.0, 0.0), LayoutSize::new(view_width, view_height));
    let clip_region = {
        let complex = webrender_traits::ComplexClipRegion::new(
            LayoutRect::new(LayoutPoint::new(0.0, 0.0),
            LayoutSize::new(view_width, view_height)),
            webrender_traits::BorderRadius::uniform(0.0),
        );

        builder.new_clip_region(&bounds, vec![complex], None)
    };

    builder.push_stacking_context(
        webrender_traits::ScrollPolicy::Scrollable,
        bounds,
        clip_region,
        0,
        &LayoutTransform::identity(),
        &LayoutTransform::identity(),
        webrender_traits::MixBlendMode::Normal,
        Vec::new(),
    );

    // Sample text to demonstrate text layout and rendering.
    let em_border = BorderSide {
        width: 1.0,
        color: ColorF::new(1.0, 0.0, 1.0, 1.0),
        style: BorderStyle::Solid,
    };
    let glyph_border = BorderSide {
        width: 1.0,
        color: ColorF::new(1.0, 0.0, 0.0, 1.0),
        style: BorderStyle::Solid,
    };
    let text_bounds = LayoutRect::new(LayoutPoint::new(0.0, 0.0), LayoutSize::new(view_width, view_height));

    // TODO: Investigate why this scaling is necessary. Rusttype says it takes font scale in pixels,
    // but glyphs rendered with the system renderer don't match the sizes produced by rusttype
    // unless we slightly tweak the rusttype scale. I used Atom displaying the Hack-Regular font at
    // 14px to compare, so if this is actually wrong blame Atom.
    //
    // Issue tracker: https://github.com/excaliburHisSheath/text-edit/issues/4
    let font_scale = Scale::uniform(FONT_SIZE_PX / PIXEL_TO_POINT);
    let v_metrics = font.v_metrics(font_scale);
    let line_height = FONT_SIZE_PX * LINE_HEIGHT;

    if let Some(scroll_to_line) = scroll_to_line {
        let line_top = scroll_to_line as f32 * line_height;
        let line_bottom = scroll_to_line as f32 * line_height + line_height;

        let view_top = editor.scroll_offset_pixels as f32 + line_height * 2.0;
        let view_bottom = editor.scroll_offset_pixels + editor.view_height_pixels as f32 - line_height * 2.0;

        // TODO: We could use a `clamp()` operation to represent this more clearly, I think?
        if view_top > line_top {
            // Scroll view upwards to match line top.
            editor.scroll_offset_pixels = line_top - line_height * 2.0;
        } else if view_bottom < line_bottom {
            // Scroll view downwards to match line bottom.
            editor.scroll_offset_pixels = line_bottom - editor.view_height_pixels as f32 + line_height * 2.0;
        }

        // Now clamp the scroll view so it doesn't go negative.
        if editor.scroll_offset_pixels < 0.0 {
            editor.scroll_offset_pixels = 0.0;
        }
    }

    // TODO: There seems to be a 5 pixel gap at the top of the window on Windows. Is this something
    // we're accidentally introducing, or is it created by webrender somehow?
    let mut origin = Point { x: 0.0, y: editor.first_line as f32 * line_height - editor.scroll_offset_pixels - 5.0 };

    for line in &editor.lines {
        origin = origin + vector(0.0, line_height);

        // Keep track of where the line ends so that we can render a cursor at the end of the line
        // if necessary.
        let mut line_end = 0.0;
        let mut last_index = 0;

        let glyphs = font
            .layout(&*line.text, font_scale, origin)
            .enumerate()
            .inspect(|&(index, ref glyph)| {
                let pos = glyph.position();
                let scaled = glyph.unpositioned();
                let h_metrics = scaled.h_metrics();

                // Update line end tracking.
                line_end = pos.x + h_metrics.advance_width;
                last_index = index + 1;

                // Draw cursors where appropriate.
                // ================================================================================
                // TODO: Is there a more efficient way to do this? E.g. could we sort the list and
                // pop the cursors as we draw them so we only have to check the next cursor?
                for cursor_col in &line.cursors {
                    if *cursor_col == index {
                        let line_middle = pos.y - v_metrics.ascent - v_metrics.descent + (v_metrics.ascent + v_metrics.descent) / 2.0;
                        let line_top = line_middle - line_height / 2.0;

                        // Draw a cursor at this col.
                        builder.push_rect(
                            LayoutRect::new(
                                LayoutPoint::new(pos.x, line_top),
                                LayoutSize::new(1.0, line_height),
                            ),
                            clip_region,
                            ColorF::new(1.0, 1.0, 1.0, 1.0),
                        );
                    }
                }

                // Debug draw bounding boxes for each glyph.
                // ================================================================================
                if !DEBUG_GLYPHS { return; }

                // Draw border based on rusttype scaled glyph.
                let rect = LayoutRect::new(
                    LayoutPoint::new(pos.x, pos.y - v_metrics.ascent - v_metrics.descent),
                    LayoutSize::new(h_metrics.advance_width, v_metrics.ascent)
                );
                builder.push_border(
                    rect,
                    clip_region,
                    em_border,
                    em_border,
                    em_border,
                    em_border,
                    webrender_traits::BorderRadius::uniform(0.0),
                );

                // Draw border based on webrender glyph dimensions.
                if let Some(bounding_box) = glyph.pixel_bounding_box() {
                    let rect = LayoutRect::new(
                        LayoutPoint::new(bounding_box.min.x as f32, bounding_box.min.y as f32),
                        LayoutSize::new(bounding_box.width() as f32, bounding_box.height() as f32),
                    );
                    builder.push_border(
                        rect,
                        clip_region,
                        glyph_border,
                        glyph_border,
                        glyph_border,
                        glyph_border,
                        webrender_traits::BorderRadius::uniform(0.0),
                    );
                }
            })
            .map(|(_, glyph)| {
                GlyphInstance {
                    index: glyph.id().0,
                    x: glyph.position().x,
                    y: glyph.position().y,
                }
            })
            .collect();

        // Render a cursor at the end of the line if necessary.
        for cursor_col in &line.cursors {
            if *cursor_col == last_index {
                let line_middle = origin.y - v_metrics.ascent - v_metrics.descent + (v_metrics.ascent + v_metrics.descent) / 2.0;
                let line_top = line_middle - line_height / 2.0;

                // Draw a cursor at this col.
                builder.push_rect(
                    LayoutRect::new(
                        LayoutPoint::new(line_end, line_top),
                        LayoutSize::new(1.0, line_height),
                    ),
                    clip_region,
                    ColorF::new(1.0, 1.0, 1.0, 1.0),
                );
            }
        }

        builder.push_text(
            text_bounds,
            webrender_traits::ClipRegion::simple(&bounds),
            glyphs,
            font_key,
            ColorF::new(0.8, 0.8, 0.8, 1.0),
            Au::from_f32_px(FONT_SIZE_PX),
            Au::from_px(0),
        );
    }

    builder.pop_stacking_context();

    builder
}

#[derive(Debug)]
struct EditorState {
    /// The total number of lines in the document.
    height_in_lines: usize,

    /// The index of the first line in `lines`.
    first_line: usize,

    /// A subset of the lines in the document.
    ///
    /// This will always have roughly a window's worth of lines, and should always be the lines
    /// currently visible based on the scroll offset.
    lines: Vec<LineContents>,

    /// The width of the editor's visible space in the window.
    ///
    /// This may be less than the total size of the window (e.g. in the case of multiple panes
    /// splitting the view), but will never be more.
    view_width_pixels: usize,

    /// The height of the editor's visible space in the window.
    ///
    /// This may be less than the total size of the window (e.g. in the case of multiple panes
    /// splitting the view), but will never be more.
    view_height_pixels: usize,

    /// The current scroll state of the editor view.
    ///
    /// 0 means that the view is at the top of the document, the value increases as the window
    /// scrolls down the document.
    ///
    /// TODO: does this setup (scrolling top-to-botton) still make sense for non-western layouts?
    scroll_offset_pixels: f32,
}

#[derive(Debug)]
struct LineContents {
    text: String,
    cursors: Vec<usize>,
    selections: Vec<(usize, usize)>,
}

/// Helper struct for updating the window when a frame is done processing.
///
/// Notifier exists so we can implement [`RenderNotifier`][RenderNotifier] for
/// [`WindowProxy`][WindowProxy]. This allows us to trigger a window repaint
/// when a frame is done rendering.
///
/// [RenderNotifier]: ./webrender//webrender_traits/trait.RenderNotifier.html
/// [WindowProxy]: glutin/struct.WindowProxy.html
struct Notifier {
    window_proxy: WindowProxy,
}

impl Notifier {
    fn new(window_proxy: WindowProxy) -> Notifier {
        Notifier {
            window_proxy: window_proxy,
        }
    }
}

impl webrender_traits::RenderNotifier for Notifier {
    fn new_frame_ready(&mut self) {
        self.window_proxy.wakeup_event_loop();
    }

    fn new_scroll_frame_ready(&mut self, _composite_needed: bool) {
        self.window_proxy.wakeup_event_loop();
    }

    fn pipeline_size_changed(&mut self, _: PipelineId, _: Option<LayoutSize>) {}
}
