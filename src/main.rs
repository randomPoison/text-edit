extern crate app_units;
extern crate glutin;
extern crate gleam;
extern crate rusttype;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate webrender;
extern crate webrender_traits;

use app_units::Au;
use gleam::gl;
use webrender_traits::*;
use rusttype::*;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

const FONT_SCALE: f32 = 14.0;
const PIXEL_TO_POINT: f32 = 0.75;
const DEBUG_GLYPHS: bool = false;

fn main() {
    // Load sample font into memory for layout purposes.
    let mut file = File::open("res/Hack-Regular.ttf").unwrap();
    let mut font_bytes = vec![];
    file.read_to_end(&mut font_bytes).unwrap();

    let font = FontCollection::from_bytes(&*font_bytes).into_font().unwrap();

    // Create a new glutin window and make its OpenGL context active.
    // ============================================================================================
    let window = glutin::WindowBuilder::new()
                .with_title("WebRender Sample")
                .with_gl(glutin::GlRequest::Specific(glutin::Api::OpenGl, (3, 2)))
                .build()
                .unwrap();

    unsafe {
        window.make_current().ok();
        gl::load_with(|symbol| window.get_proc_address(symbol) as *const _);
    }

    println!("OpenGL version {}", gl::get_string(gl::VERSION));

    // Configure and build the webrender instance.
    // ============================================================================================
    let (width, height) = window.get_inner_size().unwrap();

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
    let mut xi_stdout = BufReader::new(xi_stdout);
    let mut response = String::new();

    // Open a tab.
    // TODO: Actually track the name of the new tab and use it in future messages.
    writeln!(xi_stdin, "{}", r#"{"id":0,"method":"new_tab","params":[]}"#).expect("Failed to send message to xi-core");

    // Change the visible region of the file (no response).
    writeln!(xi_stdin, "{}", r#"{"method":"edit","params":{"method":"scroll","params":[0, 30],"tab":"0"}}"#).expect("Failed to send message to xi-core");

    // Open this file and get the lines from the file.
    writeln!(xi_stdin, "{}", r#"{"method":"edit","params":{"method":"open","params":{"filename":"src/main.rs"},"tab":"0"}}"#).expect("Failed to send message to xi-core");

    // Read response from creating the tab.
    xi_stdout.read_line(&mut response).expect("Failed to read response from xi-core");
    response.clear();

    // Read the initial update from opening the file.
    xi_stdout.read_line(&mut response).expect("Failed to read response from xi-core");
    let update_value = serde_json::from_str::<Value>(&*response).expect("Failed to parse response json");
    response.clear();

    // Parse the lines from the initial update.
    let lines = update_value
        .search("lines")
        .expect("No lines in response")
        .as_array()
        .expect("\"lines\" wasn't an array")
        .iter()
        .map(|line| {
            let line = line.as_array().expect("Line wasn't an array");
            line[0].as_str().expect("First element of line wasn't a string").trim_right().to_string()
        })
        .collect::<Vec<_>>();

    // Generate initial frame.
    let builder = build_display_lists(
        pipeline_id,
        font_key,
        &font,
        width as f32,
        height as f32,
        &*lines,
    );
    api.set_root_display_list(
        Some(root_background_color),
        epoch,
        LayoutSize::new(width as f32, height as f32),
        builder,
    );
    api.generate_frame();

    // Main event loop.
    // ============================================================================================
    for event in window.wait_events() {
        match event {
            glutin::Event::Closed => break,
            glutin::Event::KeyboardInput(_element_state, scan_code, _virtual_key_code) => {
                if scan_code == 9 {
                    break;
                }
            }
            glutin::Event::Resized(width, height) => {
                let builder = build_display_lists(
                    pipeline_id,
                    font_key,
                    &font,
                    width as f32,
                    height as f32,
                    &*lines,
                );
                api.set_root_display_list(
                    Some(root_background_color),
                    epoch,
                    LayoutSize::new(width as f32, height as f32),
                    builder,
                );
                api.generate_frame();
            }
            _ => {}//println!("Unhandled event: {:?}", event),
        }

        let (width, height) = window.get_inner_size().unwrap();

        renderer.update();
        renderer.render(DeviceUintSize::new(width, height) * hidpi_factor as u32);

        window.swap_buffers().ok();
    }
}

fn build_display_lists(
    pipeline_id: PipelineId,
    font_key: FontKey,
    font: &Font,
    width: f32,
    height: f32,
    lines: &[String],
) -> DisplayListBuilder {
    let mut builder = DisplayListBuilder::new(pipeline_id);

    let bounds = LayoutRect::new(LayoutPoint::new(0.0, 0.0), LayoutSize::new(width, height));
    let clip_region = {
        let complex = webrender_traits::ComplexClipRegion::new(
            LayoutRect::new(LayoutPoint::new(0.0, 0.0),
            LayoutSize::new(width, height)),
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
    let text_bounds = LayoutRect::new(LayoutPoint::new(0.0, 0.0), LayoutSize::new(width, height));

    // TODO: Investigate why this scaling is necessary. Rusttype says it takes font scale in pixels,
    // but glyphs rendered with the system renderer don't match the sizes produced by rusttype
    // unless we slightly tweak the rusttype scale. I used Atom displaying the Hack-Regular font at
    // 14px to compare, so if this is actually wrong blame Atom.
    let font_scale = Scale::uniform(FONT_SCALE / PIXEL_TO_POINT);
    let v_metrics = font.v_metrics(font_scale);
    let advance_height = v_metrics.ascent - v_metrics.descent + v_metrics.line_gap;

    let mut origin = Point { x: 10.0, y: 0.0 };
    for line in lines {
        origin = origin + vector(0.0, advance_height);

        let glyphs = font
            .layout(line, font_scale, origin)
            .inspect(|glyph| {
                if !DEBUG_GLYPHS { return; }

                let pos = glyph.position();
                let scaled = glyph.unpositioned();
                let h_metrics = scaled.h_metrics();

                // Draw border based on rusttype scaled glyph.
                let rect = LayoutRect::new(
                    LayoutPoint::new(pos.x, pos.y - v_metrics.ascent),
                    LayoutSize::new(h_metrics.advance_width + h_metrics.left_side_bearing, v_metrics.ascent - v_metrics.descent)
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
            .map(|glyph| {
                GlyphInstance {
                    index: glyph.id().0,
                    x: glyph.position().x,
                    y: glyph.position().y,
                }
            })
            .collect();

        builder.push_text(
            text_bounds,
            webrender_traits::ClipRegion::simple(&bounds),
            glyphs,
            font_key,
            ColorF::new(0.8, 0.8, 0.8, 1.0),
            Au::from_f32_px(FONT_SCALE),
            Au::from_px(0),
        );
    }

    builder.pop_stacking_context();

    builder
}

/// Helper struct for updating the window when a frame is done processing.
///
/// Notifier exists so we can implement [`RenderNotifier`][RenderNotifier] for
/// [`glutin::WindowProxy`][glutin::WindowProxy]. This allows us to trigger a window repaint
/// when a frame is done rendering.
///
/// [RenderNotifier]: ./webrender//webrender_traits/trait.RenderNotifier.html
/// [glutin::WindowProxy]: glutin/struct.WindowProxy.html
struct Notifier {
    window_proxy: glutin::WindowProxy,
}

impl Notifier {
    fn new(window_proxy: glutin::WindowProxy) -> Notifier {
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
