#![no_std]
#![no_main]

mod fmt;
mod test_image;

#[cfg(not(feature = "defmt"))]
use panic_halt as _;
#[cfg(feature = "defmt")]
use {defmt_rtt as _, panic_probe as _};

use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::rcc::{MSIRange, Pll, PllDiv, PllMul, PllPreDiv, PllSource, Sysclk, VoltageScale};
use embassy_time::Timer;

const WIDTH: usize = 128;
const HEIGHT: usize = 64;
const ROW_DWELL_MICROS: u64 = 150;

#[derive(Clone, Copy, Default)]
pub struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

pub struct Panel {
    pixels: [[Rgb; WIDTH]; HEIGHT],
}

impl Panel {
    pub const fn new() -> Self {
        Self {
            pixels: [[Rgb { r: 0, g: 0, b: 0 }; WIDTH]; HEIGHT],
        }
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, color: Rgb) {
        if x < WIDTH && y < HEIGHT {
            self.pixels[y][x] = color;
        }
    }

    pub fn get_pixel(&self, x: usize, y: usize) -> Rgb {
        if x < WIDTH && y < HEIGHT {
            self.pixels[y][x]
        } else {
            Rgb { r: 0, g: 0, b: 0 }
        }
    }
}

pub struct Hub75 {
    oe: Output<'static>,
    r1: Output<'static>,
    g1: Output<'static>,
    b1: Output<'static>,
    r2: Output<'static>,
    g2: Output<'static>,
    b2: Output<'static>,
    addr_a: Output<'static>,
    addr_b: Output<'static>,
    addr_c: Output<'static>,
    addr_d: Output<'static>,
    addr_e: Output<'static>,
    clk: Output<'static>,
    lat: Output<'static>,
}

impl Hub75 {
    pub fn new(p: embassy_stm32::Peripherals) -> Self {
        Self {
            oe: Output::new(p.PE6, Level::High, Speed::VeryHigh),

            r1: Output::new(p.PE7, Level::Low, Speed::VeryHigh),
            g1: Output::new(p.PE8, Level::Low, Speed::VeryHigh),
            b1: Output::new(p.PE9, Level::Low, Speed::VeryHigh),

            r2: Output::new(p.PE2, Level::Low, Speed::VeryHigh),
            g2: Output::new(p.PE3, Level::Low, Speed::VeryHigh),
            b2: Output::new(p.PE4, Level::Low, Speed::VeryHigh),

            addr_a: Output::new(p.PE10, Level::Low, Speed::VeryHigh),
            addr_b: Output::new(p.PE11, Level::Low, Speed::VeryHigh),
            addr_c: Output::new(p.PE12, Level::Low, Speed::VeryHigh),
            addr_d: Output::new(p.PE13, Level::Low, Speed::VeryHigh),
            addr_e: Output::new(p.PE14, Level::Low, Speed::VeryHigh),

            clk: Output::new(p.PE15, Level::Low, Speed::VeryHigh),
            lat: Output::new(p.PE5, Level::Low, Speed::VeryHigh),
        }
    }

    pub async fn refresh(&mut self, panel: &Panel) {
        // Each address selects a row in both the upper and lower halves.
        for y in 0..HEIGHT / 2 {
            // 1. Disable OE
            self.oe.set_level(Level::High);

            // 2. Shift pixels for both halves of the panel
            for x in 0..WIDTH {
                let upper = panel.get_pixel(x, y);
                let lower = panel.get_pixel(x, y + HEIGHT / 2);

                self.r1.set_level(if upper.r > 0 { Level::High } else { Level::Low });
                self.g1.set_level(if upper.g > 0 { Level::High } else { Level::Low });
                self.b1.set_level(if upper.b > 0 { Level::High } else { Level::Low });

                self.r2.set_level(if lower.r > 0 { Level::High } else { Level::Low });
                self.g2.set_level(if lower.g > 0 { Level::High } else { Level::Low });
                self.b2.set_level(if lower.b > 0 { Level::High } else { Level::Low });

                self.clk.set_high();
                self.clk.set_low();
            }

            // 3. Latch data
            self.lat.set_high();
            self.lat.set_low();

            // 4. Set row address
            self.addr_a.set_level(if (y & 0b00001) != 0 { Level::High } else { Level::Low });
            self.addr_b.set_level(if (y & 0b00010) != 0 { Level::High } else { Level::Low });
            self.addr_c.set_level(if (y & 0b00100) != 0 { Level::High } else { Level::Low });
            self.addr_d.set_level(if (y & 0b01000) != 0 { Level::High } else { Level::Low });
            self.addr_e.set_level(if (y & 0b10000) != 0 { Level::High } else { Level::Low });

            // 5. Enable OE
            self.oe.set_level(Level::Low);

            // 6. Keep the row lit for the configured dwell interval
            Timer::after_micros(ROW_DWELL_MICROS).await;
        }
    }
}

fn init_stm32() -> embassy_stm32::Peripherals {
    // 160 MHz
    let mut config = embassy_stm32::Config::default();
    config.rcc.msis = Some(MSIRange::RANGE_48MHZ);
    config.rcc.pll1 = Some(Pll {
        source: PllSource::MSIS,
        prediv: PllPreDiv::DIV6,
        mul: PllMul::MUL40,
        divp: None,
        divq: None,
        divr: Some(PllDiv::DIV2),
    });
    config.rcc.sys = Sysclk::PLL1_R;
    config.rcc.voltage_range = VoltageScale::RANGE1;

    embassy_stm32::init(config)
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let peripherals = init_stm32();

    let mut hub75 = Hub75::new(peripherals);

    let mut panel = Panel::new();
    test_image::load(&mut panel);

    loop {
        hub75.refresh(&panel).await;
    }
}
