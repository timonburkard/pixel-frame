use super::{Frame, HEIGHT, Rgb, WIDTH};

const TEST_IMAGE: &[u8; WIDTH * HEIGHT * 3] = include_bytes!("../assets/test_image.rgb");

pub fn load(frame: &mut Frame) {
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = (y * WIDTH + x) * 3;
            frame.set_pixel(
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
