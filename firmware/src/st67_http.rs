use embassy_time::{Duration, Instant, Timer};

use super::st67_spi::{self, St67Spi};

pub const IMAGE_RESPONSE_CAPACITY: usize = 4096;

const AT_BUFFER_SIZE: usize = 1520;
const MAX_SOCKET_READ: usize = 1200;
const HTTP_HOST: &[u8] = b"placehold.co";
const HTTP_PATH: &[u8] = b"/128x128/330000/0000FF.png";
const HTTP_PORT: &[u8] = b"443";
const IPD_PREFIX: &[u8] = b"+IPD:";
const CIPRECVDATA_PREFIX: &[u8] = b"+CIPRECVDATA:";
const HTTP_HEADER_LIMIT: usize = 1024;
const IPD_WAIT: Duration = Duration::from_millis(250);
const EMPTY_SOCKET_POLL_DELAY: Duration = Duration::from_millis(100);
const TCP_CONNECT_TIMEOUT_MS: &[u8] = b"5000";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    Transport,
    CommandNotAccepted,
    Timeout,
    AtCommandFailed,
    ResponseTooLarge,
    UnexpectedTrafficType,
    InvalidAtResponse,
    InvalidHttpResponse,
    UnsupportedHttpResponse,
    HttpStatus(u16),
    ImageTooLarge,
    SocketReadFailed,
}

pub async fn download_png(module: &mut St67Spi<'_>, response: &mut [u8; IMAGE_RESPONSE_CAPACITY]) -> Result<usize, Error> {
    let mut at_response = [0; 256];
    for command in [&b"AT+CIPMUX=1\r\n"[..], &b"AT+CIPRECVMODE=1\r\n"[..], &b"AT+CIPDINFO=1\r\n"[..]] {
        command_until(module, command, b"OK\r\n", false, &mut at_response, Duration::from_secs(3)).await?;
    }
    command_until(
        module,
        b"AT+CIPRECVBUF=0,9216\r\n",
        b"OK\r\n",
        false,
        &mut at_response,
        Duration::from_secs(3),
    )
    .await?;

    let mut dns_command = [0; 48];
    let dns_command_len = append_bytes(&mut dns_command, 0, &[&b"AT+CIPDOMAIN=\""[..], HTTP_HOST, &b"\",2\r\n"[..]])?;
    let dns_response = command_until(
        module,
        &dns_command[..dns_command_len],
        b"OK\r\n",
        false,
        &mut at_response,
        Duration::from_secs(10),
    )
    .await?;
    let (ip_start, ip_len) = quoted_value(&at_response[..dns_response], b"+CIPDOMAIN:")?;
    if ip_len == 0 || ip_len > 15 {
        return Err(Error::InvalidAtResponse);
    }
    let mut ip_address = [0; 15];
    ip_address[..ip_len].copy_from_slice(&at_response[ip_start..ip_start + ip_len]);
    if !ip_address[..ip_len].iter().all(|byte| byte.is_ascii_digit() || *byte == b'.') {
        return Err(Error::InvalidAtResponse);
    }

    command_until(
        module,
        b"AT+CIPSSLCCONF=0,0,\"\",\"\",\"\"\r\n",
        b"OK\r\n",
        false,
        &mut at_response,
        Duration::from_secs(3),
    )
    .await?;
    command_until(
        module,
        b"AT+CIPSSLCALPN=0,0\r\n",
        b"OK\r\n",
        false,
        &mut at_response,
        Duration::from_secs(3),
    )
    .await?;
    command_until(
        module,
        b"AT+CIPSSLCSNI=0,\"placehold.co\"\r\n",
        b"OK\r\n",
        false,
        &mut at_response,
        Duration::from_secs(3),
    )
    .await?;

    let mut connect_command = [0; 96];
    let connect_len = append_bytes(
        &mut connect_command,
        0,
        &[
            &b"AT+CIPSTART=0,\"SSL\",\""[..],
            &ip_address[..ip_len],
            &b"\","[..],
            HTTP_PORT,
            &b",0,,"[..],
            TCP_CONNECT_TIMEOUT_MS,
            &b"\r\n"[..],
        ],
    )?;
    command_until(
        module,
        &connect_command[..connect_len],
        b"OK\r\n",
        false,
        &mut at_response,
        HTTP_TIMEOUT,
    )
    .await?;

    let mut request = [0; 256];
    let request_len = append_bytes(
        &mut request,
        0,
        &[
            &b"GET "[..],
            HTTP_PATH,
            &b" HTTP/1.1\r\nHost: "[..],
            HTTP_HOST,
            &b"\r\nUser-Agent: pixel-frame/1\r\nAccept: image/png\r\nConnection: close\r\n\r\n"[..],
        ],
    )?;
    let mut send_command = [0; 40];
    let send_command_len = append_bytes(&mut send_command, 0, &[&b"AT+CIPSEND=0,"[..]])?;
    let send_command_len = append_decimal(&mut send_command, send_command_len, request_len)?;
    let send_command_len = append_bytes(&mut send_command, send_command_len, &[&b"\r\n"[..]])?;
    command_until(
        module,
        &send_command[..send_command_len],
        b">",
        false,
        &mut at_response,
        Duration::from_secs(5),
    )
    .await?;

    let event_len = send_socket_data(module, &request[..request_len], &mut at_response).await?;
    let deadline = Instant::now() + HTTP_TIMEOUT;
    let mut response_len = 0;
    let mut pending_socket_bytes = 0;
    let mut initial_event_len = event_len;
    let mut logged_http_headers = false;

    loop {
        let mut socket_data = [0; MAX_SOCKET_READ];
        let received = if pending_socket_bytes > 0 {
            let requested = pending_socket_bytes.min(MAX_SOCKET_READ);
            let received = pull_socket_data(module, requested, &mut socket_data).await?;
            if received == 0 || received > pending_socket_bytes {
                return Err(Error::SocketReadFailed);
            }
            #[cfg(feature = "defmt")]
            defmt::info!(
                "Pulled {} socket bytes ({} still in announced data)",
                received,
                pending_socket_bytes - received
            );
            pending_socket_bytes -= received;
            received
        } else {
            if initial_event_len > 0 {
                pending_socket_bytes = next_ipd_length(module, &at_response[..initial_event_len], deadline)
                    .await?
                    .unwrap_or(0);
                initial_event_len = 0;
            } else {
                pending_socket_bytes = next_ipd_length(module, &[], deadline).await?.unwrap_or(0);
            }

            if pending_socket_bytes > 0 {
                #[cfg(feature = "defmt")]
                defmt::info!("ST67 socket has {} bytes available", pending_socket_bytes);
                continue;
            }

            match try_pull_socket_data(module, MAX_SOCKET_READ, &mut socket_data).await? {
                Some(received) => {
                    #[cfg(feature = "defmt")]
                    defmt::info!("Retrieved {} socket bytes by polling without +IPD", received);
                    received
                }
                None => {
                    if Instant::now() >= deadline {
                        #[cfg(feature = "defmt")]
                        defmt::error!(
                            "HTTP receive deadline reached after {} bytes; partial response: {:?}",
                            response_len,
                            &response[..response_len.min(128)]
                        );
                        return Err(Error::Timeout);
                    }
                    Timer::after(EMPTY_SOCKET_POLL_DELAY).await;
                    continue;
                }
            }
        };

        if response_len + received > response.len() {
            return Err(Error::ImageTooLarge);
        }
        response[response_len..response_len + received].copy_from_slice(&socket_data[..received]);
        response_len += received;

        if let Some((header_len, content_len)) = parse_http_headers(&response[..response_len])? {
            let total_len = header_len.checked_add(content_len).ok_or(Error::ImageTooLarge)?;
            if !logged_http_headers {
                #[cfg(feature = "defmt")]
                defmt::info!(
                    "HTTP headers ({} bytes), body {} bytes, total {}: {}",
                    header_len,
                    content_len,
                    total_len,
                    core::str::from_utf8(&response[..header_len]).unwrap_or("?")
                );
                logged_http_headers = true;
            }
            if total_len > response.len() {
                return Err(Error::ImageTooLarge);
            }
            #[cfg(feature = "defmt")]
            defmt::info!("HTTP response progress: {} / {} bytes", response_len, total_len);
            if response_len >= total_len {
                response.copy_within(header_len..total_len, 0);
                return Ok(content_len);
            }
        }

        if Instant::now() >= deadline {
            #[cfg(feature = "defmt")]
            defmt::error!(
                "HTTP response deadline reached with {} bytes; partial response: {:?}",
                response_len,
                &response[..response_len.min(128)]
            );
            return Err(Error::Timeout);
        }
    }
}

async fn command_until(
    module: &mut St67Spi<'_>,
    command: &[u8],
    expected: &[u8],
    expected_at_end: bool,
    response: &mut [u8],
    timeout: Duration,
) -> Result<usize, Error> {
    command_until_inner(module, command, expected, expected_at_end, response, timeout, true).await
}

async fn command_until_inner(
    module: &mut St67Spi<'_>,
    command: &[u8],
    expected: &[u8],
    expected_at_end: bool,
    response: &mut [u8],
    timeout: Duration,
    log: bool,
) -> Result<usize, Error> {
    #[cfg(feature = "defmt")]
    if log {
        defmt::info!("ST67 AT command: {}", core::str::from_utf8(command).unwrap_or("?"));
    }

    let mut scratch = [0; AT_BUFFER_SIZE];
    let exchange = module
        .exchange(st67_spi::AT_TRAFFIC_TYPE, command, &mut scratch)
        .await
        .map_err(|_| Error::Transport)?;
    if exchange.response_type != st67_spi::AT_TRAFFIC_TYPE {
        return Err(Error::UnexpectedTrafficType);
    }
    if !exchange.tx_accepted {
        return Err(Error::CommandNotAccepted);
    }

    collect_response(
        module,
        &scratch[..exchange.response_len],
        expected,
        expected_at_end,
        response,
        timeout,
        log,
    )
    .await
}

async fn collect_response(
    module: &mut St67Spi<'_>,
    initial: &[u8],
    expected: &[u8],
    expected_at_end: bool,
    response: &mut [u8],
    timeout: Duration,
    _log_errors: bool,
) -> Result<usize, Error> {
    let mut scratch = [0; AT_BUFFER_SIZE];
    let mut response_len = append_bytes(response, 0, &[initial])?;
    let deadline = Instant::now() + timeout;

    loop {
        let current = &response[..response_len];
        let is_error = if expected_at_end {
            ends_with(current, b"ERROR\r\n")
        } else {
            ends_with(current, b"ERROR\r\n") || contains(current, b"\r\nERROR\r\n")
        };
        if is_error {
            #[cfg(feature = "defmt")]
            if _log_errors {
                defmt::error!(
                    "ST67 AT command rejected; response: {}",
                    core::str::from_utf8(current).unwrap_or("?")
                );
            }
            return Err(Error::AtCommandFailed);
        }
        if if expected_at_end {
            ends_with(current, expected)
        } else {
            contains(current, expected)
        } {
            return Ok(response_len);
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }

        if !module.is_ready() {
            Timer::after_millis(2).await;
            continue;
        }
        let exchange = match module.exchange(st67_spi::AT_TRAFFIC_TYPE, &[], &mut scratch).await {
            Ok(exchange) => exchange,
            Err(st67_spi::Error::NotReady) => continue,
            Err(_) => return Err(Error::Transport),
        };
        if exchange.response_type != st67_spi::AT_TRAFFIC_TYPE {
            return Err(Error::UnexpectedTrafficType);
        }
        response_len = append_bytes(response, response_len, &[&scratch[..exchange.response_len]])?;
    }
}

async fn send_socket_data(module: &mut St67Spi<'_>, data: &[u8], response: &mut [u8]) -> Result<usize, Error> {
    let mut scratch = [0; AT_BUFFER_SIZE];
    let exchange = module
        .exchange(st67_spi::AT_TRAFFIC_TYPE, data, &mut scratch)
        .await
        .map_err(|_| Error::Transport)?;
    if exchange.response_type != st67_spi::AT_TRAFFIC_TYPE {
        return Err(Error::UnexpectedTrafficType);
    }
    if !exchange.tx_accepted {
        return Err(Error::CommandNotAccepted);
    }
    collect_response(
        module,
        &scratch[..exchange.response_len],
        b"SEND OK\r\n",
        false,
        response,
        Duration::from_secs(5),
        true,
    )
    .await
}

async fn next_ipd_length(module: &mut St67Spi<'_>, initial_event: &[u8], deadline: Instant) -> Result<Option<usize>, Error> {
    let mut event = [0; 256];
    let mut event_len = append_bytes(&mut event, 0, &[initial_event])?;
    let mut scratch = [0; 256];
    let wait_deadline = core::cmp::min(Instant::now() + IPD_WAIT, deadline);

    loop {
        if let Some(length) = parse_ipd_length(&event[..event_len])? {
            return Ok(Some(length));
        }
        if Instant::now() >= wait_deadline {
            #[cfg(feature = "defmt")]
            defmt::info!(
                "No ST67 +IPD notification during poll window: {}",
                core::str::from_utf8(&event[..event_len]).unwrap_or("?")
            );
            return Ok(None);
        }
        if !module.is_ready() {
            Timer::after_millis(2).await;
            continue;
        }

        let exchange = match module.exchange(st67_spi::AT_TRAFFIC_TYPE, &[], &mut scratch).await {
            Ok(exchange) => exchange,
            Err(st67_spi::Error::NotReady) => continue,
            Err(_) => return Err(Error::Transport),
        };
        if exchange.response_type != st67_spi::AT_TRAFFIC_TYPE {
            return Err(Error::UnexpectedTrafficType);
        }

        if contains(&scratch[..exchange.response_len], IPD_PREFIX) {
            event_len = append_bytes(&mut event, event_len, &[&scratch[..exchange.response_len]])?;
        } else {
            #[cfg(feature = "defmt")]
            defmt::info!(
                "ST67 socket event while awaiting data ({} bytes): {}",
                exchange.response_len,
                core::str::from_utf8(&scratch[..exchange.response_len]).unwrap_or("?")
            );
            event_len = 0;
        }
    }
}

async fn try_pull_socket_data(module: &mut St67Spi<'_>, requested: usize, data: &mut [u8]) -> Result<Option<usize>, Error> {
    let mut command = [0; 48];
    let command_len = append_bytes(&mut command, 0, &[&b"AT+CIPRECVDATA=0,"[..]])?;
    let command_len = append_decimal(&mut command, command_len, requested)?;
    let command_len = append_bytes(&mut command, command_len, &[&b"\r\n"[..]])?;
    let mut response = [0; AT_BUFFER_SIZE];
    let response_len = match command_until_inner(
        module,
        &command[..command_len],
        b"\r\nOK\r\n",
        true,
        &mut response,
        Duration::from_secs(5),
        false,
    )
    .await
    {
        Ok(response_len) => response_len,
        Err(Error::AtCommandFailed) => return Ok(None),
        Err(error) => return Err(error),
    };

    let response = &response[..response_len];
    let prefix_offset = find(response, CIPRECVDATA_PREFIX).ok_or(Error::InvalidAtResponse)?;
    let length_start = prefix_offset + CIPRECVDATA_PREFIX.len();
    let comma_offset = response[length_start..]
        .iter()
        .position(|byte| *byte == b',')
        .map(|offset| length_start + offset)
        .ok_or(Error::InvalidAtResponse)?;
    let received = parse_decimal(&response[length_start..comma_offset])?;
    let data_start = comma_offset + 1;
    if received == 0 {
        return Ok(None);
    }
    if received > requested || received > data.len() || data_start + received > response.len() {
        return Err(Error::InvalidAtResponse);
    }
    data[..received].copy_from_slice(&response[data_start..data_start + received]);
    Ok(Some(received))
}

async fn pull_socket_data(module: &mut St67Spi<'_>, requested: usize, data: &mut [u8]) -> Result<usize, Error> {
    try_pull_socket_data(module, requested, data).await?.ok_or(Error::SocketReadFailed)
}

fn parse_ipd_length(response: &[u8]) -> Result<Option<usize>, Error> {
    let Some(prefix_offset) = find(response, IPD_PREFIX) else {
        return Ok(None);
    };
    let fields = &response[prefix_offset + IPD_PREFIX.len()..];
    let Some(socket_separator) = fields.iter().position(|byte| *byte == b',') else {
        return Ok(None);
    };
    if parse_decimal(&fields[..socket_separator])? != 0 {
        return Err(Error::SocketReadFailed);
    }
    let length_field = &fields[socket_separator + 1..];
    let Some(length_end) = length_field
        .iter()
        .position(|byte| *byte == b',' || *byte == b'\r' || *byte == b'\n')
    else {
        return Ok(None);
    };
    let length = parse_decimal(&length_field[..length_end])?;
    if length == 0 {
        return Err(Error::SocketReadFailed);
    }
    Ok(Some(length))
}

fn parse_http_headers(response: &[u8]) -> Result<Option<(usize, usize)>, Error> {
    let Some(header_end) = find(response, b"\r\n\r\n") else {
        if response.len() > HTTP_HEADER_LIMIT {
            return Err(Error::InvalidHttpResponse);
        }
        return Ok(None);
    };
    let header_len = header_end + 4;
    let header = &response[..header_end];
    let Some(status_line_end) = find(header, b"\r\n") else {
        return Err(Error::InvalidHttpResponse);
    };
    let status_line = &header[..status_line_end];
    let Some(status_start) = status_line.iter().position(|byte| *byte == b' ') else {
        return Err(Error::InvalidHttpResponse);
    };
    let status_end = status_start.checked_add(4).ok_or(Error::InvalidHttpResponse)?;
    let status_bytes = status_line.get(status_start + 1..status_end).ok_or(Error::InvalidHttpResponse)?;
    let status = parse_decimal(status_bytes)?;
    if status != 200 {
        return Err(Error::HttpStatus(status as u16));
    }

    let mut content_length = None;
    let mut line_start = status_line_end + 2;
    while line_start < header.len() {
        let line_end = find(&header[line_start..], b"\r\n")
            .map(|offset| line_start + offset)
            .unwrap_or(header.len());
        let line = &header[line_start..line_end];
        if let Some(colon) = line.iter().position(|byte| *byte == b':') {
            let name = &line[..colon];
            let value = trim_ascii(&line[colon + 1..]);
            if eq_ignore_ascii_case(name, b"Content-Length") {
                content_length = Some(parse_decimal(value)?);
            } else if eq_ignore_ascii_case(name, b"Transfer-Encoding") && eq_ignore_ascii_case(value, b"chunked") {
                return Err(Error::UnsupportedHttpResponse);
            }
        }
        line_start = line_end + 2;
    }

    let content_length = content_length.ok_or(Error::UnsupportedHttpResponse)?;
    let response_total = header_len.checked_add(content_length).ok_or(Error::ImageTooLarge)?;
    if response_total > IMAGE_RESPONSE_CAPACITY {
        return Err(Error::ImageTooLarge);
    }
    Ok(Some((header_len, content_length)))
}

fn quoted_value(response: &[u8], prefix: &[u8]) -> Result<(usize, usize), Error> {
    let start = find(response, prefix).ok_or(Error::InvalidAtResponse)? + prefix.len();
    let opening_quote = response[start..]
        .iter()
        .position(|byte| *byte == b'"')
        .map(|offset| start + offset + 1)
        .ok_or(Error::InvalidAtResponse)?;
    let value_len = response[opening_quote..]
        .iter()
        .position(|byte| *byte == b'"')
        .ok_or(Error::InvalidAtResponse)?;
    Ok((opening_quote, value_len))
}

fn append_bytes(destination: &mut [u8], mut used: usize, chunks: &[&[u8]]) -> Result<usize, Error> {
    for chunk in chunks {
        let end = used.checked_add(chunk.len()).ok_or(Error::ResponseTooLarge)?;
        if end > destination.len() {
            return Err(Error::ResponseTooLarge);
        }
        destination[used..end].copy_from_slice(chunk);
        used = end;
    }
    Ok(used)
}

fn append_decimal(destination: &mut [u8], mut used: usize, mut value: usize) -> Result<usize, Error> {
    let mut digits = [0; 10];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    while count > 0 {
        count -= 1;
        used = append_bytes(destination, used, &[&digits[count..count + 1]])?;
    }
    Ok(used)
}

fn parse_decimal(bytes: &[u8]) -> Result<usize, Error> {
    if bytes.is_empty() {
        return Err(Error::InvalidAtResponse);
    }
    let mut value = 0usize;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(Error::InvalidAtResponse);
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add((byte - b'0') as usize))
            .ok_or(Error::InvalidAtResponse)?;
    }
    Ok(value)
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

fn ends_with(bytes: &[u8], suffix: &[u8]) -> bool {
    bytes.len() >= suffix.len() && bytes[bytes.len() - suffix.len()..] == *suffix
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}
