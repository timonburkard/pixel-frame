use super::{HEIGHT, Panel, Rgb, WIDTH, st67_http, st67_spi::St67Spi};

const DISPLAY_HEIGHT: usize = HEIGHT * 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    Download(st67_http::Error),
    InvalidPng,
    WrongDimensions,
}

pub async fn load(module: &mut St67Spi<'_>, panel_top: &mut Panel, panel_bottom: &mut Panel) -> Result<usize, Error> {
    let mut png_bytes = [0; st67_http::IMAGE_RESPONSE_CAPACITY];
    let png_len = st67_http::download_png(module, &mut png_bytes).await.map_err(Error::Download)?;

    let header = minipng::decode_png_header(&png_bytes[..png_len]).map_err(|_| Error::InvalidPng)?;
    if header.width() as usize != WIDTH || header.height() as usize != DISPLAY_HEIGHT {
        return Err(Error::WrongDimensions);
    }

    let mut decoded = [0; WIDTH * DISPLAY_HEIGHT * 4 + DISPLAY_HEIGHT]; // incl. filter bytes
    let mut image = minipng::decode_png(&png_bytes[..png_len], &mut decoded).map_err(|_| Error::InvalidPng)?;
    image.convert_to_rgba8bpc().map_err(|_| Error::InvalidPng)?;

    let pixels = image.pixels();
    for y in 0..DISPLAY_HEIGHT {
        for x in 0..WIDTH {
            let offset = (y * WIDTH + x) * 4;
            let alpha = pixels[offset + 3] as u16;
            let color = Rgb {
                r: (pixels[offset] as u16 * alpha / 255) as u8,
                g: (pixels[offset + 1] as u16 * alpha / 255) as u8,
                b: (pixels[offset + 2] as u16 * alpha / 255) as u8,
            };
            if y < HEIGHT {
                panel_top.set_pixel(x, y, color);
            } else {
                panel_bottom.set_pixel(x, y - HEIGHT, color);
            }
        }
    }

    Ok(png_len)
}
