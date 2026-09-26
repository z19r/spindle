//! How expensive is one animation frame?
//!
//! #148 asks whether an animated GIF preview can rebuild a protocol
//! per frame on the review screen's tick, or whether it has to cap
//! the animation. Two costs matter and they are not the same: making
//! the protocol, which is nearly free, and rendering it, which is
//! where the encode and the terminal write actually happen.
//!
//! Run with `cargo run --release --example measure_gif_protocol`.

use std::time::Instant;

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::StatefulImage;

const FRAMES: usize = 60;

fn main() {
  // A preview pane on a normal terminal: about half the width, most
  // of the height. In cells, because that is what the protocol is
  // asked to fill.
  let area = Rect::new(0, 0, 60, 30);

  // Every protocol, not just the one this terminal happens to
  // negotiate: the question is whether the *expensive* ones can keep
  // up, and they are exactly the ones a dev box may not be running.
  for proto in [
    ProtocolType::Halfblocks,
    ProtocolType::Sixel,
    ProtocolType::Kitty,
    ProtocolType::Iterm2,
  ] {
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(proto);
    let name = format!("{proto:?}");
    // Source frames at a size a real GIF preview would be.
    let frames: Vec<image::DynamicImage> = (0..FRAMES)
      .map(|i| {
        let mut buf = image::RgbImage::new(480, 360);
        for (x, y, p) in buf.enumerate_pixels_mut() {
          let t = (i * 4) as u32;
          *p = image::Rgb([
            ((x + t) % 256) as u8,
            ((y + t) % 256) as u8,
            ((x + y + t) % 256) as u8,
          ]);
        }
        image::DynamicImage::ImageRgb8(buf)
      })
      .collect();

    let t = Instant::now();
    let mut protocols: Vec<_> = frames
      .iter()
      .cloned()
      .map(|f| picker.new_resize_protocol(f))
      .collect();
    let build = t.elapsed();

    let backend = TestBackend::new(area.width, area.height);
    let mut term = Terminal::new(backend).expect("terminal");

    // First render of each protocol: this is the one that encodes.
    let t = Instant::now();
    for p in &mut protocols {
      term
        .draw(|f| {
          f.render_stateful_widget(StatefulImage::new(), area, p)
        })
        .expect("draw");
    }
    let first = t.elapsed();

    // Second pass over the same protocols, same area: what a replay
    // of already-built frames costs.
    let t = Instant::now();
    for p in &mut protocols {
      term
        .draw(|f| {
          f.render_stateful_widget(StatefulImage::new(), area, p)
        })
        .expect("draw");
    }
    let replay = t.elapsed();

    // Rebuilding from the source image every time, which is what a
    // naive per-tick implementation would do.
    let t = Instant::now();
    for f in &frames {
      let mut p = picker.new_resize_protocol(f.clone());
      term
        .draw(|f| {
          f.render_stateful_widget(StatefulImage::new(), area, &mut p)
        })
        .expect("draw");
    }
    let rebuild = t.elapsed();

    println!(
      "{name}, {FRAMES} frames at 480x360 into {}x{} cells",
      area.width, area.height
    );
    println!(
      "  build protocols      {:>9.3?}  ({:.3?}/frame)",
      build,
      build / FRAMES as u32
    );
    println!(
      "  first render (encode){:>9.3?}  ({:.3?}/frame)",
      first,
      first / FRAMES as u32
    );
    println!(
      "  replay built frames  {:>9.3?}  ({:.3?}/frame)",
      replay,
      replay / FRAMES as u32
    );
    println!(
      "  rebuild each tick    {:>9.3?}  ({:.3?}/frame)",
      rebuild,
      rebuild / FRAMES as u32
    );
    // What one frame costs to *send*, which is the cost a fast CPU
    // hides and a slow pty or an ssh session does not. The graphics
    // protocols carry the whole image as escape sequences in the
    // cells, so the buffer's own symbols are the payload.
    let mut p = picker.new_resize_protocol(frames[0].clone());
    term
      .draw(|f| {
        f.render_stateful_widget(StatefulImage::new(), area, &mut p)
      })
      .expect("draw");
    let bytes: usize = term
      .backend()
      .buffer()
      .content()
      .iter()
      .map(|c| c.symbol().len())
      .sum();

    let budget = std::time::Duration::from_millis(80);
    println!(
      "  review tick is {budget:?}; rebuild uses {:.1}% of it",
      (rebuild / FRAMES as u32).as_secs_f64() / budget.as_secs_f64()
        * 100.0
    );
    println!(
      "  payload              {:>7.1} KiB/frame ({:.1} KiB/s at 12fps)",
      bytes as f64 / 1024.0,
      bytes as f64 / 1024.0 * 12.0
    );
  }
}
