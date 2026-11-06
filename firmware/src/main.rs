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
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{RoundTo, Timer};
use embassy_stm32::{bind_interrupts, pac, peripherals};

const WIDTH: usize = 128;
const HEIGHT: usize = 64;
const CLK_MASK: u16 = 1 << 15;
const OE_MASK: u16 = 1 << 6;
const LAT_MASK: u16 = 1 << 5;
const INTENSITY_PHASES: u8 = 4; // Number of phases for intensity control
const WORDS_PER_ROW: usize = WIDTH * 2 + 5;
const SCAN_WORDS: usize = HEIGHT / 2 * WORDS_PER_ROW * INTENSITY_PHASES as usize;
const DMA_REQUEST_TIM6: u8 = 4;
const DMA_WORD_RATE: u32 = 16_000_000;
const MAX_DMA_DESCRIPTOR_WORDS: usize = u16::MAX as usize / core::mem::size_of::<u16>();
const DMA_DESCRIPTOR_COUNT: usize = SCAN_WORDS.div_ceil(MAX_DMA_DESCRIPTOR_WORDS);

bind_interrupts!(struct Irqs {
    GPDMA1_CHANNEL0 => embassy_stm32::dma::InterruptHandler<peripherals::GPDMA1_CH0>;
});

#[repr(C, align(4))]
#[derive(Clone, Copy)]
struct DmaLinkedListItem {
    tr1: pac::gpdma::regs::ChTr1,
    tr2: pac::gpdma::regs::ChTr2,
    br1: pac::gpdma::regs::ChBr1,
    sar: u32,
    dar: u32,
    llr: pac::gpdma::regs::ChLlr,
}

impl DmaLinkedListItem {
    const ZERO: Self = Self {
        tr1: pac::gpdma::regs::ChTr1(0),
        tr2: pac::gpdma::regs::ChTr2(0),
        br1: pac::gpdma::regs::ChBr1(0),
        sar: 0,
        dar: 0,
        llr: pac::gpdma::regs::ChLlr(0),
    };
}

static mut SCAN_MEMORY: [u16; SCAN_WORDS] = [0; SCAN_WORDS];
static mut DMA_LINKED_LIST: [DmaLinkedListItem; DMA_DESCRIPTOR_COUNT] = [DmaLinkedListItem::ZERO; DMA_DESCRIPTOR_COUNT];

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
    _timer: Timer<'static, peripherals::TIM6>,
    _dma_channel: embassy_stm32::dma::Channel<'static>,
    _oe: Output<'static>,
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
    _clk: Output<'static>,
    _lat: Output<'static>,
}

impl Hub75 {
    pub fn new(p: embassy_stm32::Peripherals) -> Self {
        Self {
            _timer: Timer::new(p.TIM6),
            _dma_channel: embassy_stm32::dma::Channel::new(p.GPDMA1_CH0, Irqs),
            _oe: Output::new(p.PE6, Level::High, Speed::VeryHigh),

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

            _clk: Output::new(p.PE15, Level::Low, Speed::VeryHigh),
            _lat: Output::new(p.PE5, Level::Low, Speed::VeryHigh),
        }
    }

    pub fn refresh_memory(&mut self, panel: &Panel, memory: &mut [u16; SCAN_WORDS]) {
        let mut index: usize = 0;
        let mut addr_oe_state = OE_MASK; // Start with OE disabled

        for intensity_phase in 0..INTENSITY_PHASES {
            let intensity_threshold = ((intensity_phase as u16 + 1) * 255 / (INTENSITY_PHASES as u16 + 1)) as u8;

            // Each address selects a row in both the upper and lower halves.
            for y in 0..HEIGHT / 2 {
                // 1. Shift pixels for both halves of the panel
                let upper_row = &panel.pixels[y];
                let lower_row = &panel.pixels[y + HEIGHT / 2];

                for x in 0..WIDTH {
                    let upper = upper_row[x];
                    let lower = lower_row[x];
                    let set_mask = (((upper.r > intensity_threshold) as u16) << 7)
                        | (((upper.g > intensity_threshold) as u16) << 8)
                        | (((upper.b > intensity_threshold) as u16) << 9)
                        | (((lower.r > intensity_threshold) as u16) << 2)
                        | (((lower.g > intensity_threshold) as u16) << 3)
                        | (((lower.b > intensity_threshold) as u16) << 4);

                    memory[index] = addr_oe_state | set_mask;
                    memory[index + 1] = addr_oe_state | set_mask | CLK_MASK;
                    index += 2;
                }

                // 2. Disable OE
                memory[index] = memory[index - 1] | OE_MASK;
                index += 1;

                // 3. Latch data
                memory[index] = memory[index - 1] | LAT_MASK;
                memory[index + 1] = memory[index - 1];
                index += 2;

                // 4. Set row address
                addr_oe_state = ((y as u16) << 10) | OE_MASK;
                memory[index] = addr_oe_state;
                index += 1;

                // 5. Enable OE
                addr_oe_state &= !OE_MASK;
                memory[index] = addr_oe_state;
                index += 1;
            }
        }

        assert_eq!(index, SCAN_WORDS);
    }

    pub fn start_dma(&self, memory: &'static [u16; SCAN_WORDS]) {
        let gpioe_odr = pac::GPIOE.odr().as_ptr() as *mut u16;
        let list = unsafe { &mut *core::ptr::addr_of_mut!(DMA_LINKED_LIST) };
        let list_ptr = list.as_mut_ptr();
        let list_base_address = list_ptr as u32;
        assert_eq!(list_base_address & 0b11, 0);

        for (index, item) in list.iter_mut().enumerate() {
            let start_word = index * MAX_DMA_DESCRIPTOR_WORDS;
            let chunk_words = (SCAN_WORDS - start_word).min(MAX_DMA_DESCRIPTOR_WORDS);
            let source = unsafe { memory.as_ptr().add(start_word) };
            let next_index = (index + 1) % DMA_DESCRIPTOR_COUNT;
            let next_address = unsafe { list_ptr.add(next_index) } as u32;

            assert_eq!(next_address >> 16, list_base_address >> 16);
            assert_eq!(next_address & 0b11, 0);

            item.tr1.set_sdw(pac::gpdma::vals::Dw::HALF_WORD);
            item.tr1.set_ddw(pac::gpdma::vals::Dw::HALF_WORD);
            item.tr1.set_sinc(true);
            item.tr1.set_dinc(false);
            item.tr1.set_sap(pac::gpdma::vals::Ap::PORT0);
            item.tr1.set_dap(pac::gpdma::vals::Ap::PORT1);

            item.tr2.set_dreq(pac::gpdma::vals::Dreq::DESTINATION_PERIPHERAL);
            item.tr2.set_reqsel(DMA_REQUEST_TIM6);
            item.br1.set_bndt((chunk_words * core::mem::size_of::<u16>()) as u16);
            item.sar = source as u32;
            item.dar = gpioe_odr as u32;

            item.llr.set_ut1(true);
            item.llr.set_ut2(true);
            item.llr.set_ub1(true);
            item.llr.set_usa(true);
            item.llr.set_uda(true);
            item.llr.set_ull(true);
            item.llr.set_la((next_address as u16) >> 2);
        }

        let channel = pac::GPDMA1.ch(0);
        channel.cr().write(|w| w.set_reset(true));
        channel.fcr().write(|w| {
            w.set_dtef(true);
            w.set_htf(true);
            w.set_suspf(true);
            w.set_tcf(true);
            w.set_tof(true);
            w.set_ulef(true);
            w.set_usef(true);
        });
        channel.lbar().write(|w| w.set_lba((list_base_address >> 16) as u16));
        channel.br1().write(|w| w.set_bndt(0));
        channel.llr().write(|w| {
            w.set_ut1(true);
            w.set_ut2(true);
            w.set_ub1(true);
            w.set_usa(true);
            w.set_uda(true);
            w.set_ull(true);
            w.set_la((list_base_address as u16) >> 2);
        });
        channel.tr3().write(|_| {});
        channel.cr().write(|w| {
            w.set_prio(pac::gpdma::vals::Prio::HIGH);
            w.set_dteie(true);
            w.set_uleie(true);
            w.set_useie(true);
        });
        channel.cr().modify(|w| w.set_en(true));

        self._timer.set_frequency(Hertz(DMA_WORD_RATE), RoundTo::Slower);
        self._timer.generate_update_event();
        pac::TIM6.dier().modify(|w| w.set_ude(true));
        self._timer.start();
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

    let memory = unsafe { &mut *core::ptr::addr_of_mut!(SCAN_MEMORY) };
    hub75.refresh_memory(&panel, memory);
    hub75.start_dma(memory);

    loop {
        cortex_m::asm::wfi();
    }
}
