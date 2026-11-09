#![no_std]
#![no_main]

mod remote_image;
mod st67_http;
pub mod st67_spi;
#[cfg(feature = "defmt")]
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
use embassy_time::{Duration, Instant, Timer as AsyncTimer};

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

impl Default for Panel {
    fn default() -> Self {
        Self::new()
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

async fn drain_st67_ready_messages(module: &mut st67_spi::St67Spi<'_>) -> bool {
    const IDLE_TIMEOUT: Duration = Duration::from_millis(250);
    const TOTAL_TIMEOUT: Duration = Duration::from_secs(2);
    const MAX_MESSAGES: usize = 16;

    let total_deadline = Instant::now() + TOTAL_TIMEOUT;
    let mut idle_deadline = Instant::now() + IDLE_TIMEOUT;
    let mut response = [0; 64];
    let mut messages = 0;

    while Instant::now() < total_deadline && Instant::now() < idle_deadline && messages < MAX_MESSAGES {
        if !module.is_ready() {
            AsyncTimer::after_millis(2).await;
            continue;
        }

        match module.exchange(st67_spi::AT_TRAFFIC_TYPE, &[], &mut response).await {
            Ok(_exchange) => {
                #[cfg(feature = "defmt")]
                defmt::info!(
                    "ST67 queued message type {} len {}: {}",
                    _exchange.response_type,
                    _exchange.response_len,
                    core::str::from_utf8(&response[.._exchange.response_len]).unwrap_or("?")
                );
                if response[.._exchange.response_len]
                    .windows(b"+CW:GOTIP".len())
                    .any(|window| window == b"+CW:GOTIP")
                {
                    return true;
                }
                messages += 1;
                idle_deadline = Instant::now() + IDLE_TIMEOUT;
            }
            Err(st67_spi::Error::NotReady) => {
                AsyncTimer::after_millis(2).await;
            }
            Err(error) => {
                #[cfg(feature = "defmt")]
                defmt::error!("ST67 queued-message poll failed: {:?}", error);
                #[cfg(not(feature = "defmt"))]
                panic!("ST67 queued-message poll failed: {:?}", error);
                #[cfg(feature = "defmt")]
                break;
            }
        }
    }

    if module.is_ready() {
        #[cfg(feature = "defmt")]
        defmt::warn!("ST67 READY remained asserted after polling limit");
    }
    false
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
enum SsidQueryError {
    Transport,
    UnexpectedTrafficType,
    CommandNotAccepted,
    NotConnected,
    Timeout,
    WifiIpTimeout,
    ResponseTooLarge,
    InvalidResponse,
}

async fn wait_for_wifi_ip(module: &mut st67_spi::St67Spi<'_>) -> Result<(), SsidQueryError> {
    const WIFI_GOT_IP: &[u8] = b"+CW:GOTIP";
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut event = [0; 64];
    let mut event_len = 0;
    let mut response = [0; 64];

    #[cfg(feature = "defmt")]
    defmt::info!("Waiting for ST67 Wi-Fi connection and IP address");

    while Instant::now() < deadline {
        if !module.is_ready() {
            AsyncTimer::after_millis(2).await;
            continue;
        }

        match module.exchange(st67_spi::AT_TRAFFIC_TYPE, &[], &mut response).await {
            Ok(exchange) if exchange.response_type == st67_spi::AT_TRAFFIC_TYPE => {
                #[cfg(feature = "defmt")]
                defmt::info!(
                    "ST67 Wi-Fi event ({} bytes): {}",
                    exchange.response_len,
                    core::str::from_utf8(&response[..exchange.response_len]).unwrap_or("?")
                );

                if exchange.response_len >= event.len() {
                    event_len = 0;
                } else if event_len + exchange.response_len > event.len() {
                    let keep = event.len() - exchange.response_len;
                    event.copy_within(event_len - keep..event_len, 0);
                    event_len = keep;
                }
                event[event_len..event_len + exchange.response_len].copy_from_slice(&response[..exchange.response_len]);
                event_len += exchange.response_len;

                if event[..event_len].windows(WIFI_GOT_IP.len()).any(|window| window == WIFI_GOT_IP) {
                    return Ok(());
                }
            }
            Ok(_) => return Err(SsidQueryError::UnexpectedTrafficType),
            Err(st67_spi::Error::NotReady) => {}
            Err(_) => return Err(SsidQueryError::Transport),
        }
    }

    Err(SsidQueryError::WifiIpTimeout)
}

async fn query_connected_ssid(module: &mut st67_spi::St67Spi<'_>, ssid: &mut [u8; 32]) -> Result<usize, SsidQueryError> {
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
    const PREFIX: &[u8] = b"+CWJAP:";
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut collected = [0; 256];
    let mut collected_len = 0;
    let mut chunk = [0; 256];

    let exchange = module
        .exchange(st67_spi::AT_TRAFFIC_TYPE, b"AT+CWJAP?\r\n", &mut chunk)
        .await
        .map_err(|_| SsidQueryError::Transport)?;
    if exchange.response_type != st67_spi::AT_TRAFFIC_TYPE {
        return Err(SsidQueryError::UnexpectedTrafficType);
    }
    if !exchange.tx_accepted {
        return Err(SsidQueryError::CommandNotAccepted);
    }
    let mut chunk_len = exchange.response_len;
    if chunk_len > chunk.len() {
        return Err(SsidQueryError::ResponseTooLarge);
    }
    #[cfg(feature = "defmt")]
    defmt::info!(
        "ST67 CWJAP initial response ({} bytes): {}",
        chunk_len,
        core::str::from_utf8(&chunk[..chunk_len]).unwrap_or("?")
    );

    loop {
        if collected_len + chunk_len > collected.len() {
            return Err(SsidQueryError::ResponseTooLarge);
        }
        collected[collected_len..collected_len + chunk_len].copy_from_slice(&chunk[..chunk_len]);
        collected_len += chunk_len;

        let response = &collected[..collected_len];
        if response.windows(7).any(|window| window == b"ERROR\r\n") {
            return Err(SsidQueryError::NotConnected);
        }
        let has_ssid = response.windows(PREFIX.len()).any(|window| window == PREFIX);
        let has_ok = response.windows(4).any(|window| window == b"OK\r\n");
        if has_ssid && has_ok {
            let prefix_offset = response
                .windows(PREFIX.len())
                .position(|window| window == PREFIX)
                .ok_or(SsidQueryError::InvalidResponse)?;
            let mut start = prefix_offset + PREFIX.len();
            while response.get(start).is_some_and(|byte| *byte == b' ' || *byte == b'\t') {
                start += 1;
            }
            if response.get(start) != Some(&b'"') {
                return Err(SsidQueryError::InvalidResponse);
            }
            start += 1;
            let end = response[start..]
                .iter()
                .position(|byte| *byte == b'"')
                .map(|offset| start + offset)
                .ok_or(SsidQueryError::InvalidResponse)?;
            let ssid_len = end - start;
            if ssid_len == 0 || ssid_len > ssid.len() {
                return Err(SsidQueryError::InvalidResponse);
            }
            ssid[..ssid_len].copy_from_slice(&response[start..end]);
            return Ok(ssid_len);
        }

        if Instant::now() >= deadline {
            #[cfg(feature = "defmt")]
            defmt::error!(
                "ST67 CWJAP timed out; collected {} bytes: {}",
                collected_len,
                core::str::from_utf8(response).unwrap_or("?")
            );
            return Err(SsidQueryError::Timeout);
        }
        if module.is_ready() {
            match module.exchange(st67_spi::AT_TRAFFIC_TYPE, &[], &mut chunk).await {
                Ok(exchange) if exchange.response_type == st67_spi::AT_TRAFFIC_TYPE => {
                    chunk_len = exchange.response_len;
                    #[cfg(feature = "defmt")]
                    defmt::info!(
                        "ST67 CWJAP response chunk ({} bytes): {}",
                        chunk_len,
                        core::str::from_utf8(&chunk[..chunk_len]).unwrap_or("?")
                    );
                }
                Ok(_) => return Err(SsidQueryError::UnexpectedTrafficType),
                Err(st67_spi::Error::NotReady) => chunk_len = 0,
                Err(_) => return Err(SsidQueryError::Transport),
            }
        } else {
            chunk_len = 0;
            AsyncTimer::after_millis(2).await;
        }
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let peripherals = init_stm32();

    let mut st67_spi = st67_spi::St67Spi::new(
        peripherals.SPI1,
        peripherals.PA5,
        peripherals.PA7,
        peripherals.PA6,
        peripherals.PD14,
        peripherals.PE13,
        peripherals.PE11,
    );
    st67_spi.power_on().await;
    let mut has_wifi_ip = drain_st67_ready_messages(&mut st67_spi).await;
    if !has_wifi_ip {
        match wait_for_wifi_ip(&mut st67_spi).await {
            Ok(()) => has_wifi_ip = true,
            Err(error) => {
                #[cfg(feature = "defmt")]
                defmt::error!("Timed out waiting for ST67 Wi-Fi IP: {:?}", error);
                #[cfg(not(feature = "defmt"))]
                panic!("Timed out waiting for ST67 Wi-Fi IP: {:?}", error);
            }
        }
    }

    if has_wifi_ip {
        let mut connected_ssid = [0; 32];
        match query_connected_ssid(&mut st67_spi, &mut connected_ssid).await {
            Ok(_ssid_len) => {
                #[cfg(feature = "defmt")]
                defmt::info!(
                    "Connected to Wi-Fi SSID: {}",
                    core::str::from_utf8(&connected_ssid[.._ssid_len]).unwrap_or("?")
                );
            }
            Err(error) => {
                #[cfg(feature = "defmt")]
                defmt::error!("Failed to query connected Wi-Fi SSID: {:?}", error);
                #[cfg(not(feature = "defmt"))]
                panic!("Failed to query connected Wi-Fi SSID: {:?}", error);
            }
        }
    }

    let mut hub75_top = Hub75::new(embassy_stm32::dma::Channel::new(peripherals.GPDMA1_CH0, Irqs), 0, GpioPort::G);
    let mut hub75_bottom = Hub75::new(embassy_stm32::dma::Channel::new(peripherals.GPDMA1_CH1, Irqs), 1, GpioPort::F);

    let mut panel_top = Panel::new();
    let mut panel_bottom = Panel::new();

    match remote_image::load(&mut st67_spi, &mut panel_top, &mut panel_bottom).await {
        Ok(_png_len) => {
            #[cfg(feature = "defmt")]
            defmt::info!("Downloaded and decoded {}-byte remote PNG", _png_len);
        }
        Err(error) => {
            #[cfg(feature = "defmt")]
            defmt::error!("Remote image load failed: {:?}; using built-in test image", error);
            #[cfg(not(feature = "defmt"))]
            panic!("Remote image load failed: {:?}", error);
            #[cfg(feature = "defmt")]
            test_image::load(&mut panel_top, &mut panel_bottom);
        }
    }

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
