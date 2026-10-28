#![no_std]
#![no_main]

mod fmt;

#[cfg(not(feature = "defmt"))]
use panic_halt as _;
#[cfg(feature = "defmt")]
use {defmt_rtt as _, panic_probe as _};

use embassy_executor::Spawner;
use embassy_stm32::{
    gpio::{Level, Output, Speed},
    rcc::Sysclk,
};
use embassy_time::{Duration, Timer};
use fmt::info;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();

    // Use HSI16 instead of the default 4 MHz MSI clock.
    config.rcc.hsi = true;
    config.rcc.sys = Sysclk::HSI;

    let p = embassy_stm32::init(config);

    let r1 = Output::new(p.PE7, Level::Low, Speed::VeryHigh);
    let g1 = Output::new(p.PE8, Level::Low, Speed::VeryHigh);
    let b1 = Output::new(p.PE9, Level::Low, Speed::VeryHigh);

    let addr_a = Output::new(p.PE10, Level::Low, Speed::VeryHigh);
    let addr_b = Output::new(p.PE11, Level::High, Speed::VeryHigh);
    let addr_c = Output::new(p.PE12, Level::Low, Speed::VeryHigh);
    let addr_d = Output::new(p.PE13, Level::Low, Speed::VeryHigh);
    let addr_e = Output::new(p.PE14, Level::Low, Speed::VeryHigh);

    let clk = Output::new(p.PE15, Level::Low, Speed::VeryHigh);
    let lat = Output::new(p.PE5, Level::Low, Speed::VeryHigh);
    let oe = Output::new(p.PE6, Level::Low, Speed::VeryHigh);
}
