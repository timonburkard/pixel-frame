use super::{HEIGHT, Panel, Rgb, WIDTH};

const TEST_IMAGE: &[u8; WIDTH * HEIGHT * 3 * 2] = include_bytes!("../assets/kingfisher_128x128.rgb");

pub fn load(panel_top: &mut Panel, panel_bottom: &mut Panel) {
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = (y * WIDTH + x) * 3;
            panel_top.set_pixel(
                x,
                y,
                Rgb {
                    r: TEST_IMAGE[offset],
                    g: TEST_IMAGE[offset + 1],
                    b: TEST_IMAGE[offset + 2],
                },
            );
        }
    }

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = TEST_IMAGE.len() / 2 + (y * WIDTH + x) * 3;
            panel_bottom.set_pixel(
                x,
                y,
                Rgb {
                    r: TEST_IMAGE[offset],
                    g: TEST_IMAGE[offset + 1],
                    b: TEST_IMAGE[offset + 2],
                },
            );
        }
    }
}
