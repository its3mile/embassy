//! This example uses the RP Pico W board Wifi chip (cyw43).
//! Connects to Wifi network and makes a web request to get the current time.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]

use core::fmt::Write;
use core::str::from_utf8;
use cyw43::JoinOptions;
use cyw43_pio::PioSpi;
use defmt::{error, info, unwrap};
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::{Config, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::spi;
use embassy_rp::spi::Spi;
use embassy_time::{Delay, Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use epd_waveshare::{epd3in7::*, prelude::*};
use heapless::String; // For the `write!` macro
use panic_probe as _;
use rand::{Rng, RngCore};
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::request::Method;
use serde::Deserialize;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _, serde_json_core};

const DISPLAY_FREQ: u32 = 16_000_000;
// Mobile hotspot so okay for now
const WIFI_NETWORK: &str = "Galaxy S10e22a7"; // TODO: From env var
const WIFI_PASSWORD: &str = "lkfs5033"; // TODO: From env var

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});

// Use embedded-graphics to create a bitmap to show text
fn display_text(text: &str) -> Display3in7 {
    use embedded_graphics::{
        mono_font::{ascii::FONT_10X20, MonoTextStyle},
        prelude::*,
        primitives::PrimitiveStyle,
        text::{Alignment, Text},
    };

    // Create a Display buffer to draw on, specific for this ePaper
    let mut display = Display3in7::default();

    // Landscape mode, USB plug to the right
    display.set_rotation(DisplayRotation::Rotate270);

    // Change the background from the default black to white
    let _ = display
        .bounding_box()
        .into_styled(PrimitiveStyle::with_fill(Color::White))
        .draw(&mut display);

    // Draw text on the buffer
    Text::with_alignment(
        text,
        display.bounding_box().center() + Point::new(1, 0),
        MonoTextStyle::new(&FONT_10X20, Color::Black),
        Alignment::Center,
    )
    .draw(&mut display)
    .unwrap();
    Text::with_alignment(
        text,
        display.bounding_box().center() + Point::new(0, 1),
        MonoTextStyle::new(&FONT_10X20, Color::Black),
        Alignment::Center,
    )
    .draw(&mut display)
    .unwrap();

    display
}

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO0, 0, DMA_CH0>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut rng = RoscRng;

    let fw = include_bytes!("../../../../cyw43-firmware/43439A0.bin");
    let clm = include_bytes!("../../../../cyw43-firmware/43439A0_clm.bin");
    // To make flashing faster for development, you may want to flash the firmwares independently
    // at hardcoded addresses, instead of baking them into the program with `include_bytes!`:
    //     probe-rs download 43439A0.bin --binary-format bin --chip RP2040 --base-address 0x10100000
    //     probe-rs download 43439A0_clm.bin --binary-format bin --chip RP2040 --base-address 0x10140000
    // let fw = unsafe { core::slice::from_raw_parts(0x10100000 as *const u8, 230321) };
    // let clm = unsafe { core::slice::from_raw_parts(0x10140000 as *const u8, 4752) };

    let pin_reset: Output<'_> = Output::new(p.PIN_12, Level::Low);
    let pin_cs = Output::new(p.PIN_9, Level::High);
    let pin_data_cmd: Output<'_> = Output::new(p.PIN_8, Level::Low);
    let pin_spi_sclk = p.PIN_10;
    let pin_spi_mosi = p.PIN_11;
    let pin_busy = Input::new(p.PIN_13, embassy_rp::gpio::Pull::None);

    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnFirstTransition;
    display_config.polarity = spi::Polarity::IdleLow;

    let spi_bus = Spi::new_blocking_txonly(p.SPI1, pin_spi_sclk, pin_spi_mosi, display_config.clone());
    let mut spi_device = ExclusiveDevice::new(spi_bus, pin_cs, Delay);

    // // Setup the EPD driver
    let mut epd_driver = EPD3in7::new(&mut spi_device, pin_busy, pin_data_cmd, pin_reset, &mut Delay, None).unwrap(); // Force unwrap, as there is nothing that can be done if this errors out

    // Create splash drawing
    let splash = display_text("its3mile/london-pi-tube");

    // Render splash drawing
    epd_driver
        .update_and_display_frame(&mut spi_device, splash.buffer(), &mut Delay)
        .unwrap();

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(&mut pio.common, pio.sm0, pio.irq0, cs, p.PIN_24, p.PIN_29, p.DMA_CH0);

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw).await;
    unwrap!(spawner.spawn(cyw43_task(runner)));

    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    let config = Config::dhcpv4(Default::default());

    // Generate random seed
    let seed = rng.next_u64();

    // Init network stack
    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(net_device, config, RESOURCES.init(StackResources::new()), seed);

    unwrap!(spawner.spawn(net_task(runner)));

    loop {
        match control
            .join(WIFI_NETWORK, JoinOptions::new(WIFI_PASSWORD.as_bytes()))
            .await
        {
            Ok(_) => break,
            Err(err) => {
                let mut buffer: String<32> = String::new();
                let _ = write!(&mut buffer, "join failed with status={}", err.status);
                let message: &str = &buffer;
                let message = display_text(message);
                epd_driver
                    .update_and_display_frame(&mut spi_device, message.buffer(), &mut Delay)
                    .unwrap();
                info!("join failed with status={}", err.status);
            }
        }
    }

    // Wait for DHCP, not necessary when using static IP
    info!("waiting for DHCP...");
    while !stack.is_config_up() {
        Timer::after_millis(100).await;
    }
    info!("DHCP is now up!");

    info!("waiting for link up...");
    while !stack.is_link_up() {
        Timer::after_millis(500).await;
    }
    info!("Link is up!");

    info!("waiting for stack to be up...");
    stack.wait_config_up().await;
    info!("Stack is up!");

    // And now we can use it!

    let messages = [
        (
            "English - Merry Chrismas!",
            "https://worldtimeapi.org/api/timezone/Europe/London",
        ),
        (
            "Spanish - ¡Feliz Navidad!",
            "https://worldtimeapi.org/api/timezone/Europe/Madrid",
        ),
        (
            "French - Joyeux Noël!",
            "https://worldtimeapi.org/api/timezone/Europe/Paris",
        ),
        (
            "German - Frohe Weihnachten!",
            "https://worldtimeapi.org/api/timezone/Europe/Berlin",
        ),
        (
            "Italian - Buon Natale!",
            "https://worldtimeapi.org/api/timezone/Europe/Rome",
        ),
        (
            "Portuguese - Feliz Natal!",
            "https://worldtimeapi.org/api/timezone/Europe/Lisbon",
        ),
        (
            "Romanian - Crăciun Fericit!",
            "https://worldtimeapi.org/api/timezone/Europe/Bucharest",
        ),
        (
            "Russian - Счастливого Рождества! (Schastlivogo Rozhdestva!)",
            "https://worldtimeapi.org/api/timezone/Europe/Moscow",
        ),
        (
            "Swedish - God Jul!",
            "https://worldtimeapi.org/api/timezone/Europe/Stockholm",
        ),
        // ("Norwegian - God Jul!", "??"),
        // ("Danish - Glædelig Jul!", "??"),
        (
            "Finnish - Hyvää Joulua!",
            "https://worldtimeapi.org/api/timezone/Europe/Helsinki",
        ),
        // ("Icelandic - Gleðileg Jól!", "??"),
        (
            "Polish - Wesołych Świąt!",
            "https://worldtimeapi.org/api/timezone/Europe/Warsaw",
        ),
        // ("Dutch - Vrolijk Kerstfeest!", "??"),
        // ("Croatian - Sretan Božić!", "??"),
        (
            "Czech - Veselé Vánoce!",
            "https://worldtimeapi.org/api/timezone/Europe/Prague",
        ),
        (
            "Japanese - メリークリスマス！ (Merīkurisumasu!)",
            "https://worldtimeapi.org/api/timezone/Asia/Tokyo",
        ),
        (
            "Chinese - 圣诞节快乐! (Shèngdàn jié kuàilè!)",
            "https://worldtimeapi.org/api/timezone/Asia/Shanghai",
        ),
        (
            "Korean - 메리 크리스마스! (Meli Keuliseumaseu!)",
            "https://worldtimeapi.org/api/timezone/Asia/Seoul",
        ),
        // ("Latin - Felicem Natalem Christi!", "??"),
        (
            "Irish - Nollaig Shona!",
            "https://worldtimeapi.org/api/timezone/Europe/Dublin",
        ),
    ];

    loop {
        Timer::after(Duration::from_secs(2)).await;

        let mut rx_buffer = [0; 8192];
        let mut tls_read_buffer = [0; 16640];
        let mut tls_write_buffer = [0; 16640];

        let client_state = TcpClientState::<1, 1024, 1024>::new();
        let tcp_client = TcpClient::new(stack, &client_state);
        let dns_client = DnsSocket::new(stack);
        let tls_config = TlsConfig::new(seed, &mut tls_read_buffer, &mut tls_write_buffer, TlsVerify::None);

        let mut http_client = HttpClient::new_with_tls(&tcp_client, &dns_client, tls_config);
        let message_index = rng.gen_range(0..messages.len());
        let message_value = messages[message_index];
        let url = message_value.1;
        info!("connecting to {}", &url);

        let mut request = match http_client.request(Method::GET, &url).await {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to make HTTP request: {:?}", e);
                let message = display_text("Failed to make HTTP request");
                epd_driver
                    .update_and_display_frame(&mut spi_device, message.buffer(), &mut Delay)
                    .unwrap();
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };

        let response = match request.send(&mut rx_buffer).await {
            Ok(resp) => resp,
            Err(_e) => {
                error!("Failed to send HTTP request");
                let message = display_text("Failed to send HTTP request");
                epd_driver
                    .update_and_display_frame(&mut spi_device, message.buffer(), &mut Delay)
                    .unwrap();
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };

        let body = match from_utf8(response.body().read_to_end().await.unwrap()) {
            Ok(b) => b,
            Err(_e) => {
                error!("Failed to read response body");
                let message = display_text("Failed to read response body");
                epd_driver
                    .update_and_display_frame(&mut spi_device, message.buffer(), &mut Delay)
                    .unwrap();
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };
        info!("Response body: {:?}", &body);

        // parse the response body and update the RTC

        #[derive(Deserialize)]
        struct ApiResponse<'a> {
            datetime: &'a str,
            // other fields as needed
        }

        let bytes = body.as_bytes();
        match serde_json_core::de::from_slice::<ApiResponse>(bytes) {
            Ok((output, _used)) => {
                info!("Datetime: {:?}", output.datetime);
                let mut buffer: String<256> = String::new();
                let _ = write!(&mut buffer, "{}\n{}", message_value.0, output.datetime);
                let message: &str = &buffer;
                let message = display_text(message);
                Timer::after(Duration::from_secs(3)).await;
                epd_driver
                    .update_and_display_frame(&mut spi_device, message.buffer(), &mut Delay)
                    .unwrap();
            }
            Err(_error) => {
                error!("Failed to parse response body");
                let mut buffer: String<256> = String::new();
                let _ = write!(&mut buffer, "{}\nCouldn't get time :()", message_value.0,);
                let message: &str = &buffer;
                let message = display_text(message);
                epd_driver
                    .update_and_display_frame(&mut spi_device, message.buffer(), &mut Delay)
                    .unwrap();
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }
}
