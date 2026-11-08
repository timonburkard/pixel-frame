#![no_std]
#![no_main]

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

const PIN_ADDR_A: u8 = 0;
const PIN_ADDR_B: u8 = 1;
const PIN_ADDR_C: u8 = 2;
const PIN_ADDR_D: u8 = 3;
const PIN_ADDR_E: u8 = 4;
const PIN_R1: u8 = 5;
const PIN_G1: u8 = 6;
const PIN_B1: u8 = 7;
const PIN_R2: u8 = 8;
const PIN_G2: u8 = 9;
const PIN_B2: u8 = 10;
const PIN_CLK: u8 = 12;
const PIN_LAT: u8 = 13;
const PIN_OE: u8 = 14;

const R1_MASK: u16 = 1 << PIN_R1;
const G1_MASK: u16 = 1 << PIN_G1;
const B1_MASK: u16 = 1 << PIN_B1;
const R2_MASK: u16 = 1 << PIN_R2;
const G2_MASK: u16 = 1 << PIN_G2;
const B2_MASK: u16 = 1 << PIN_B2;
const CLK_MASK: u16 = 1 << PIN_CLK;
const LAT_MASK: u16 = 1 << PIN_LAT;
const OE_MASK: u16 = 1 << PIN_OE;

const GPIO_PINS_PER_PORT: u8 = 16;

const INTENSITY_PHASES: u8 = 16; // Number of phases for color intensity control

const DMA_WORDS_PER_ROW: usize = WIDTH * 2 + 5;
const DMA_SCAN_WORDS: usize = HEIGHT / 2 * DMA_WORDS_PER_ROW * INTENSITY_PHASES as usize;
const DMA_REQUEST_TIM6: u8 = 4;
const DMA_WORD_RATE: u32 = 16_000_000;
const DMA_MAX_DESCRIPTOR_WORDS: usize = u16::MAX as usize / core::mem::size_of::<u16>();
const DMA_DESCRIPTOR_COUNT: usize = DMA_SCAN_WORDS.div_ceil(DMA_MAX_DESCRIPTOR_WORDS);
const DMA_CHANNEL_COUNT: usize = 2;

bind_interrupts!(struct Irqs {
    GPDMA1_CHANNEL0 => embassy_stm32::dma::InterruptHandler<peripherals::GPDMA1_CH0>;
    GPDMA1_CHANNEL1 => embassy_stm32::dma::InterruptHandler<peripherals::GPDMA1_CH1>;
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

static mut SCAN_MEMORY_TOP: [u16; DMA_SCAN_WORDS] = [0; DMA_SCAN_WORDS];
static mut SCAN_MEMORY_BOTTOM: [u16; DMA_SCAN_WORDS] = [0; DMA_SCAN_WORDS];
static mut DMA_LINKED_LISTS: [[DmaLinkedListItem; DMA_DESCRIPTOR_COUNT]; DMA_CHANNEL_COUNT] =
    [[DmaLinkedListItem::ZERO; DMA_DESCRIPTOR_COUNT]; DMA_CHANNEL_COUNT];

#[derive(Clone, Copy, Default)]
pub struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

pub struct Panel {
    pixels: [[Rgb; WIDTH]; HEIGHT],
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum GpioPort {
    F = 5,
    G = 6,
}

fn gpio_output(port: GpioPort, pin: u8, level: Level) -> Output<'static> {
    let pin = unsafe { embassy_stm32::gpio::AnyPin::steal(port as u8 * GPIO_PINS_PER_PORT + pin) };
    Output::new(pin, level, Speed::VeryHigh)
}

fn gpio_output_register(port: GpioPort) -> *mut u16 {
    match port {
        GpioPort::F => pac::GPIOF.odr().as_ptr() as *mut u16,
        GpioPort::G => pac::GPIOG.odr().as_ptr() as *mut u16,
    }
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
    _dma_channel: embassy_stm32::dma::Channel<'static>,
    dma_channel_index: usize,
    gpio_odr: *mut u16,
    _addr_a: Output<'static>,
    _addr_b: Output<'static>,
    _addr_c: Output<'static>,
    _addr_d: Output<'static>,
    _addr_e: Output<'static>,
    _r1: Output<'static>,
    _g1: Output<'static>,
    _b1: Output<'static>,
    _r2: Output<'static>,
    _g2: Output<'static>,
    _b2: Output<'static>,
    _clk: Output<'static>,
    _lat: Output<'static>,
    _oe: Output<'static>,
}

impl Hub75 {
    pub fn new(dma_channel: embassy_stm32::dma::Channel<'static>, dma_channel_index: usize, gpio_port: GpioPort) -> Self {
        Self {
            _dma_channel: dma_channel,
            dma_channel_index,
            gpio_odr: gpio_output_register(gpio_port),
            _addr_a: gpio_output(gpio_port, PIN_ADDR_A, Level::Low),
            _addr_b: gpio_output(gpio_port, PIN_ADDR_B, Level::Low),
            _addr_c: gpio_output(gpio_port, PIN_ADDR_C, Level::Low),
            _addr_d: gpio_output(gpio_port, PIN_ADDR_D, Level::Low),
            _addr_e: gpio_output(gpio_port, PIN_ADDR_E, Level::Low),
            _r1: gpio_output(gpio_port, PIN_R1, Level::Low),
            _g1: gpio_output(gpio_port, PIN_G1, Level::Low),
            _b1: gpio_output(gpio_port, PIN_B1, Level::Low),
            _r2: gpio_output(gpio_port, PIN_R2, Level::Low),
            _g2: gpio_output(gpio_port, PIN_G2, Level::Low),
            _b2: gpio_output(gpio_port, PIN_B2, Level::Low),
            _clk: gpio_output(gpio_port, PIN_CLK, Level::Low),
            _lat: gpio_output(gpio_port, PIN_LAT, Level::Low),
            _oe: gpio_output(gpio_port, PIN_OE, Level::High),
        }
    }

    pub fn refresh_memory(&mut self, panel: &Panel, memory: &mut [u16; DMA_SCAN_WORDS]) {
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
                    let set_mask = if upper.r > intensity_threshold { R1_MASK } else { 0 }
                        | if upper.g > intensity_threshold { G1_MASK } else { 0 }
                        | if upper.b > intensity_threshold { B1_MASK } else { 0 }
                        | if lower.r > intensity_threshold { R2_MASK } else { 0 }
                        | if lower.g > intensity_threshold { G2_MASK } else { 0 }
                        | if lower.b > intensity_threshold { B2_MASK } else { 0 };

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
                addr_oe_state = y as u16 | OE_MASK;
                memory[index] = addr_oe_state;
                index += 1;

                // 5. Enable OE
                addr_oe_state &= !OE_MASK;
                memory[index] = addr_oe_state;
                index += 1;
            }
        }

        assert_eq!(index, DMA_SCAN_WORDS);
    }

    pub fn start_dma(&self, memory: &'static [u16; DMA_SCAN_WORDS]) {
        assert!(self.dma_channel_index < DMA_CHANNEL_COUNT);
        let lists = core::ptr::addr_of_mut!(DMA_LINKED_LISTS).cast::<[DmaLinkedListItem; DMA_DESCRIPTOR_COUNT]>();
        let list = unsafe { &mut *lists.add(self.dma_channel_index) };
        let list_ptr = list.as_mut_ptr();
        let list_base_address = list_ptr as u32;
        assert_eq!(list_base_address & 0b11, 0);

        for (index, item) in list.iter_mut().enumerate() {
            let start_word = index * DMA_MAX_DESCRIPTOR_WORDS;
            let chunk_words = (DMA_SCAN_WORDS - start_word).min(DMA_MAX_DESCRIPTOR_WORDS);
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
            item.dar = self.gpio_odr as u32;

            item.llr.set_ut1(true);
            item.llr.set_ut2(true);
            item.llr.set_ub1(true);
            item.llr.set_usa(true);
            item.llr.set_uda(true);
            item.llr.set_ull(true);
            item.llr.set_la((next_address as u16) >> 2);
        }

        let channel = pac::GPDMA1.ch(self.dma_channel_index);
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
    }
}

fn start_refresh_timer(timer: &mut Timer<'static, peripherals::TIM6>) {
    timer.set_frequency(Hertz(DMA_WORD_RATE), RoundTo::Slower);
    timer.generate_update_event();
    pac::TIM6.dier().modify(|w| w.set_ude(true));
    timer.start();
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

    let mut hub75_top = Hub75::new(embassy_stm32::dma::Channel::new(peripherals.GPDMA1_CH0, Irqs), 0, GpioPort::G);
    let mut hub75_bottom = Hub75::new(embassy_stm32::dma::Channel::new(peripherals.GPDMA1_CH1, Irqs), 1, GpioPort::F);

    let mut panel_top = Panel::new();
    let mut panel_bottom = Panel::new();

    test_image::load(&mut panel_top, &mut panel_bottom);

    let memory_top = unsafe { &mut *core::ptr::addr_of_mut!(SCAN_MEMORY_TOP) };
    hub75_top.refresh_memory(&panel_top, memory_top);
    hub75_top.start_dma(memory_top);

    let memory_bottom = unsafe { &mut *core::ptr::addr_of_mut!(SCAN_MEMORY_BOTTOM) };
    hub75_bottom.refresh_memory(&panel_bottom, memory_bottom);
    hub75_bottom.start_dma(memory_bottom);

    let mut refresh_timer = Timer::new(peripherals.TIM6);
    start_refresh_timer(&mut refresh_timer);

    loop {
        cortex_m::asm::wfi();
    }
}
