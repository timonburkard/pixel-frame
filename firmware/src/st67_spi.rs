use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::mode::Blocking;
use embassy_stm32::spi::{BitOrder, Config, Mode, Phase, Polarity, Spi, mode::Master};
use embassy_stm32::time::Hertz;
use embassy_stm32::{Peri, peripherals};
use embassy_time::{Duration, Instant, Timer};

const HEADER_SIZE: usize = 8;
const MAX_PAYLOAD_SIZE: usize = 1520;
const MAX_FRAME_SIZE: usize = HEADER_SIZE + MAX_PAYLOAD_SIZE;
pub const AT_TRAFFIC_TYPE: u8 = 0;
const TRAFFIC_TYPE_COUNT: u8 = 5;
const SPI_HEADER_MAGIC: u16 = 0x55AA;
const TXN_READY_TIMEOUT_MS: u64 = 2_000;
const HEADER_ACK_TIMEOUT_MS: u64 = 100;

pub struct St67Spi<'d> {
    spi: Spi<'d, Blocking, Master>,
    chip_select: Output<'d>,
    chip_enable: Output<'d>,
    ready: Input<'d>,
    tx_frame: [u8; MAX_FRAME_SIZE],
    rx_frame: [u8; MAX_FRAME_SIZE],
    rx_stalled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    PayloadTooLarge,
    InvalidTrafficType,
    NotReady,
    Timeout,
    InvalidHeader,
    ResponseTooLarge,
    Spi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Exchange {
    pub response_len: usize,
    pub response_type: u8,
    pub tx_accepted: bool,
}

impl<'d> St67Spi<'d> {
    pub fn new(
        spi: Peri<'d, peripherals::SPI1>,
        sck: Peri<'d, peripherals::PA5>,
        mosi: Peri<'d, peripherals::PA7>,
        miso: Peri<'d, peripherals::PA6>,
        chip_select: Peri<'d, peripherals::PD14>,
        ready: Peri<'d, peripherals::PE13>,
        chip_enable: Peri<'d, peripherals::PE11>,
    ) -> Self {
        let mut config = Config::default();
        config.mode = Mode {
            polarity: Polarity::IdleLow,
            phase: Phase::CaptureOnFirstTransition,
        };
        config.bit_order = BitOrder::MsbFirst;
        config.frequency = Hertz(8_000_000);

        Self {
            spi: Spi::new_blocking(spi, sck, mosi, miso, config),
            chip_select: Output::new(chip_select, Level::Low, Speed::VeryHigh),
            chip_enable: Output::new(chip_enable, Level::Low, Speed::VeryHigh),
            ready: Input::new(ready, Pull::None),
            tx_frame: [0; MAX_FRAME_SIZE],
            rx_frame: [0; MAX_FRAME_SIZE],
            rx_stalled: false,
        }
    }

    pub async fn power_on(&mut self) {
        self.chip_select.set_low();
        self.chip_enable.set_high();
        Timer::after_millis(1_000).await;
    }

    pub fn is_ready(&self) -> bool {
        self.ready.is_high()
    }

    pub async fn exchange(&mut self, traffic_type: u8, payload: &[u8], response: &mut [u8]) -> Result<Exchange, Error> {
        if traffic_type >= TRAFFIC_TYPE_COUNT {
            return Err(Error::InvalidTrafficType);
        }
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(Error::PayloadTooLarge);
        }

        self.chip_select.set_high();
        let result = self.exchange_selected(traffic_type, payload, response).await;
        self.chip_select.set_low();
        result
    }

    async fn exchange_selected(&mut self, traffic_type: u8, payload: &[u8], response: &mut [u8]) -> Result<Exchange, Error> {
        if payload.is_empty() && !self.ready.is_high() {
            return Err(Error::NotReady);
        }
        if !payload.is_empty() {
            self.wait_for_level(true, TXN_READY_TIMEOUT_MS).await?;
        }

        let send_payload = !payload.is_empty() && !self.rx_stalled;
        let sent_type = if send_payload { traffic_type } else { AT_TRAFFIC_TYPE };
        let sent_len = if send_payload { payload.len() } else { 0 };
        let first_transfer_len = align_four(HEADER_SIZE + sent_len);

        self.tx_frame[..first_transfer_len].fill(0);
        encode_header(&mut self.tx_frame[..HEADER_SIZE], sent_len, sent_type);
        self.tx_frame[HEADER_SIZE..HEADER_SIZE + sent_len].copy_from_slice(if send_payload { payload } else { &[] });

        self.spi
            .blocking_transfer(&mut self.rx_frame[..first_transfer_len], &self.tx_frame[..first_transfer_len])
            .map_err(|_| Error::Spi)?;

        let (response_type, response_len, rx_stalled) = decode_header(&self.rx_frame[..HEADER_SIZE])?;
        if response_type >= TRAFFIC_TYPE_COUNT || response_len > MAX_PAYLOAD_SIZE {
            return Err(Error::InvalidHeader);
        }
        self.rx_stalled = rx_stalled;

        let complete_frame_len = align_four(HEADER_SIZE + response_len);
        if complete_frame_len > first_transfer_len {
            let remaining = complete_frame_len - first_transfer_len;
            self.tx_frame[..remaining].fill(0);
            self.spi
                .blocking_transfer(
                    &mut self.rx_frame[first_transfer_len..complete_frame_len],
                    &self.tx_frame[..remaining],
                )
                .map_err(|_| Error::Spi)?;
        }

        self.wait_for_level(false, HEADER_ACK_TIMEOUT_MS).await?;

        if response_len > response.len() {
            return Err(Error::ResponseTooLarge);
        }
        response[..response_len].copy_from_slice(&self.rx_frame[HEADER_SIZE..HEADER_SIZE + response_len]);

        Ok(Exchange {
            response_len,
            response_type,
            tx_accepted: payload.is_empty() || (send_payload && !rx_stalled),
        })
    }

    async fn wait_for_level(&mut self, high: bool, timeout_ms: u64) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while self.ready.is_high() != high {
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            Timer::after_millis(1).await;
        }
        Ok(())
    }
}

fn align_four(length: usize) -> usize {
    (length + 3) & !3
}

fn encode_header(header: &mut [u8], payload_len: usize, traffic_type: u8) {
    header[..HEADER_SIZE].fill(0);
    header[0..2].copy_from_slice(&SPI_HEADER_MAGIC.to_le_bytes());
    header[2..4].copy_from_slice(&(payload_len as u16).to_le_bytes());
    header[5] = traffic_type;
}

fn decode_header(header: &[u8]) -> Result<(u8, usize, bool), Error> {
    if header.len() < HEADER_SIZE || u16::from_le_bytes([header[0], header[1]]) != SPI_HEADER_MAGIC {
        return Err(Error::InvalidHeader);
    }

    let payload_len = u16::from_le_bytes([header[2], header[3]]) as usize;
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(Error::InvalidHeader);
    }
    Ok((header[5], payload_len, header[4] & (1 << 2) != 0))
}
