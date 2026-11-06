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
use embassy_stm32::pac::{self, gpio::regs::Bsrr};
use embassy_stm32::rcc::{MSIRange, Pll, PllDiv, PllMul, PllPreDiv, PllSource, Sysclk, VoltageScale};

const WIDTH: usize = 128;
const HEIGHT: usize = 64;
const RGB_DATA_MASK: u32 = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 7) | (1 << 8) | (1 << 9);
const CLK_MASK: u32 = 1 << 15;
const ADDR_MASK: u32 = 0b1_1111 << 10;
const INTENSITY_PHASES: u8 = 4; // Number of phases for intensity control

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
    _r1: Output<'static>,
    _g1: Output<'static>,
    _b1: Output<'static>,
    _r2: Output<'static>,
    _g2: Output<'static>,
    _b2: Output<'static>,
    _addr_a: Output<'static>,
    _addr_b: Output<'static>,
    _addr_c: Output<'static>,
    _addr_d: Output<'static>,
    _addr_e: Output<'static>,
    clk: Output<'static>,
    lat: Output<'static>,
    intensity_phase: u8,
}

impl Hub75 {
    pub fn new(p: embassy_stm32::Peripherals) -> Self {
        Self {
            oe: Output::new(p.PE6, Level::High, Speed::VeryHigh),

            _r1: Output::new(p.PE7, Level::Low, Speed::VeryHigh),
            _g1: Output::new(p.PE8, Level::Low, Speed::VeryHigh),
            _b1: Output::new(p.PE9, Level::Low, Speed::VeryHigh),

            _r2: Output::new(p.PE2, Level::Low, Speed::VeryHigh),
            _g2: Output::new(p.PE3, Level::Low, Speed::VeryHigh),
            _b2: Output::new(p.PE4, Level::Low, Speed::VeryHigh),

            _addr_a: Output::new(p.PE10, Level::Low, Speed::VeryHigh),
            _addr_b: Output::new(p.PE11, Level::Low, Speed::VeryHigh),
            _addr_c: Output::new(p.PE12, Level::Low, Speed::VeryHigh),
            _addr_d: Output::new(p.PE13, Level::Low, Speed::VeryHigh),
            _addr_e: Output::new(p.PE14, Level::Low, Speed::VeryHigh),

            clk: Output::new(p.PE15, Level::Low, Speed::VeryHigh),
            lat: Output::new(p.PE5, Level::Low, Speed::VeryHigh),

            intensity_phase: 0, // 0 ... INTENSITY_PHASES - 1
        }
    }

    pub async fn refresh(&mut self, panel: &Panel) {
        self.intensity_phase = (self.intensity_phase + 1) % (INTENSITY_PHASES);

        let intensity_threshold = ((self.intensity_phase as u16 + 1) * 255 / (INTENSITY_PHASES as u16 + 1)) as u8;

        // Each address selects a row in both the upper and lower halves.
        for y in 0..HEIGHT / 2 {
            // 1. Shift pixels for both halves of the panel
            let upper_row = &panel.pixels[y];
            let lower_row = &panel.pixels[y + HEIGHT / 2];

            for x in 0..WIDTH {
                let upper = upper_row[x];
                let lower = lower_row[x];
                let set_mask = (((upper.r > intensity_threshold) as u32) << 7)
                    | (((upper.g > intensity_threshold) as u32) << 8)
                    | (((upper.b > intensity_threshold) as u32) << 9)
                    | (((lower.r > intensity_threshold) as u32) << 2)
                    | (((lower.g > intensity_threshold) as u32) << 3)
                    | (((lower.b > intensity_threshold) as u32) << 4);
                let reset_mask = (RGB_DATA_MASK & !set_mask) | CLK_MASK;
                pac::GPIOE.bsrr().write_value(Bsrr(set_mask | (reset_mask << 16)));

                self.clk.set_high();
            }

            self.clk.set_low();

            // 2. Disable OE
            self.oe.set_level(Level::High);

            // 3. Latch data
            self.lat.set_high();
            self.lat.set_low();

            // 4. Set row address
            let addr_set_mask = (y as u32) << 10;
            let addr_reset_mask = ADDR_MASK & !addr_set_mask;
            pac::GPIOE.bsrr().write_value(Bsrr(addr_set_mask | (addr_reset_mask << 16)));

            // 5. Enable OE
            self.oe.set_level(Level::Low);
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
