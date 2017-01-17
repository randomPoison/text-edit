extern crate app_units;
extern crate euclid;
extern crate glutin;
extern crate gleam;
extern crate rusttype;
extern crate webrender;
extern crate webrender_traits;

use app_units::Au;
use gleam::gl;
use webrender_traits::*;
use rusttype::*;
use std::fs::File;
use std::io::Read;

static TEST_STRING: &'static str = "Mammon slept.";

fn main() {
    // Load sample font into memory for layout purposes.
    let mut file = File::open("res/FreeSans.ttf").unwrap();
    let mut font_bytes = vec![];
    file.read_to_end(&mut font_bytes).unwrap();

    let font = FontCollection::from_bytes(&*font_bytes).into_font().expect("Unable to load font from res/FreeSans.ttf");

    // Create a new glutin window and make its OpenGL context active.
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
    // =============================================================================================
    let (width, height) = window.get_inner_size().unwrap();
    println!("width: {}, height: {}", width, height);

    let opts = webrender::RendererOptions {
        device_pixel_ratio: window.hidpi_factor(),
        debug: true,
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
    let root_background_color = ColorF::new(0.3, 0.1, 0.1, 1.0);

    let pipeline_id = PipelineId(0, 0);
    api.set_root_pipeline(pipeline_id);

    let font_key = api.add_raw_font(font_bytes.clone());

    let builder = build_display_lists(pipeline_id, font_key, &font, width as f32, height as f32);
    api.set_root_display_list(
        Some(root_background_color),
        epoch,
        LayoutSize::new(width as f32, height as f32),
        builder);

    for event in window.wait_events() {
        renderer.update();

        renderer.render(DeviceUintSize::new(width * window.hidpi_factor() as u32, height * window.hidpi_factor() as u32));

        window.swap_buffers().ok();

        match event {
            glutin::Event::Closed => break,
            glutin::Event::KeyboardInput(_element_state, scan_code, _virtual_key_code) => {
                if scan_code == 9 {
                    break;
                }
            }
            glutin::Event::Resized(width, height) => {
                println!("width: {}, height: {}", width, height);
                // Rebuild the layout for the new window size.
                // TODO: Can we just update the old builder instead of recreating it?
                let builder = build_display_lists(pipeline_id, font_key, &font, width as f32, height as f32);
                api.set_root_display_list(
                    Some(root_background_color),
                    epoch,
                    LayoutSize::new(width as f32, height as f32),
                    builder);
            }
            _ => {}//println!("Unhandled event: {:?}", event),
        }
    }
}

fn build_display_lists(
    pipeline_id: PipelineId,
    font_key: FontKey,
    font: &Font,
    width: f32,
    height: f32,
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

    // Yellow rectangle that takes up most of the scren except for 50px around the edges.
    builder.push_rect(
        LayoutRect::new(LayoutPoint::new(50.0, 50.0),
        LayoutSize::new(width - 100.0, height - 100.0)),
        clip_region,
        ColorF::new(1.0, 1.0, 0.0, 1.0),
    );

    // Green rectangle sitting towards the middle of the window.
    builder.push_rect(
        LayoutRect::new(LayoutPoint::new(250.0, 150.0),
        LayoutSize::new(100.0, 100.0)),
        clip_region,
        ColorF::new(0.0, 1.0, 0.0, 1.0),
    );
    let border_side = webrender_traits::BorderSide {
        width: 3.0,
        color: ColorF::new(0.0, 0.0, 1.0, 1.0),
        style: webrender_traits::BorderStyle::Dashed,
    };
    builder.push_border(
        LayoutRect::new(LayoutPoint::new(250.0, 150.0),
        LayoutSize::new(100.0, 100.0)),
        clip_region,
        border_side,
        border_side,
        border_side,
        border_side,
        webrender_traits::BorderRadius::uniform(0.0),
    );

    let device_pixel_ratio: f32 = 1.0;
    let font_size = 16.0;
    let em_size = font_size / 16.0;
    let design_units_per_em = 1000;
    let design_units_per_pixel = design_units_per_em as f32 / 16.0;
    let scaled_design_units_to_pixels = (em_size * device_pixel_ratio) / design_units_per_pixel;

    // Sample text to demonstrate text layout and rendering.
    let text_bounds = LayoutRect::new(LayoutPoint::new(100.0, 200.0), LayoutSize::new(700.0, 700.0));
    let glyphs = font
        // .layout(TEST_STRING, Scale::uniform(32.0), Point { x: 0.0, y: 32.0 })
        .glyphs_for(TEST_STRING.chars())
        .scan(0.0, |x, glyph| {
            let scaled = glyph.scaled(Scale::uniform(32.0));
            let width = scaled.h_metrics().advance_width;
            let next = scaled.positioned(Point { x: *x, y: 32.0 });
            *x += width;
            Some(next)
        })
        .map(|glyph| {
            GlyphInstance {
                index: glyph.id().0,
                x: glyph.position().x * scaled_design_units_to_pixels,
                y: glyph.position().y,
            }
        })
        .collect();

    builder.push_text(
        text_bounds,
        webrender_traits::ClipRegion::simple(&bounds),
        glyphs,
        font_key,
        ColorF::new(0.0, 0.0, 1.0, 1.0),
        Au::from_px(32),
        Au::from_px(0),
    );

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
