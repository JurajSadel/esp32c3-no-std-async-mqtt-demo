#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

// peripherals-related imports
use hal::{
    clock::{ClockControl, CpuClock},
    i2c::I2C,
    peripherals::{Interrupt, Peripherals},
    prelude::{_fugit_RateExtU32, *},
    systimer::SystemTimer,
    timer::TimerGroup,
    Rng, IO, {embassy, interrupt},
};

// Wifi-related imports
use embedded_svc::wifi::{ClientConfiguration, Configuration, Wifi};
use esp_wifi::{
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState},
    {initialize, EspWifiInitFor},
};

// embassy related imports
use embassy_executor::Spawner;
use embassy_net::{
    tcp::TcpSocket,
    {dns::DnsQueryType, Config, Stack, StackResources},
};
use embassy_time::{Duration, Timer};

// Temperature sensor related imports
use crate::bmp180_async::Bmp180;
mod bmp180_async;

// MQTT related imports
use rust_mqtt::{
    client::{client::MqttClient, client_config::ClientConfig},
    packet::v5::reason_codes::ReasonCode,
    utils::rng_generator::CountingRng,
};

// Formatting related imports
use core::fmt::Write;
use heapless::String;

use static_cell::make_static;

use esp_backtrace as _;
use esp_println::println;

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

// maintains wifi connection, when it disconnects it tries to reconnect
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("start connection task");
    println!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        match esp_wifi::wifi::get_wifi_state() {
            WifiState::StaConnected => {
                // wait until we're no longer connected
                controller.wait_for_event(WifiEvent::StaDisconnected).await;
                Timer::after(Duration::from_millis(5000)).await
            }
            _ => {}
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: SSID.try_into().unwrap(),
                password: PASSWORD.try_into().unwrap(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            println!("Starting wifi");
            controller.start().await.unwrap();
            println!("Wifi started!");
        }
        println!("About to connect...");

        match controller.connect().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

// A background task, to process network events - when new packets, they need to processed, embassy-net, wraps smoltcp
#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::configure(system.clock_control, CpuClock::Clock160MHz).freeze();

    let timer_group0 = TimerGroup::new(peripherals.TIMG0, &clocks);

    let init = initialize(
        EspWifiInitFor::Wifi,
        SystemTimer::new(peripherals.SYSTIMER).alarm0,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    )
    .expect("Failed to initialize Wifi");

    embassy::init(&clocks, timer_group0);
    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    let wifi = peripherals.WIFI;
    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    // Create a new peripheral object with the described wiring
    // and standard I2C clock speed
    let i2c0 = I2C::new(
        peripherals.I2C0,
        io.pins.gpio1,
        io.pins.gpio2,
        100u32.kHz(),
        &clocks,
    );

    let config = Config::dhcpv4(Default::default());

    let seed = 1234; // very random, very secure seed

    // Init network stack
    let stack = &*make_static!(Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>::new()),
        seed
    ));

    interrupt::enable(Interrupt::I2C_EXT0, interrupt::Priority::Priority1)
        .expect("Invalid Interrupt Priority Error");

    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(&stack)).ok();

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    //wait until wifi connected
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address); //dhcp IP address
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        Timer::after(Duration::from_millis(1_000)).await;

        let mut socket = TcpSocket::new(&stack, &mut rx_buffer, &mut tx_buffer);

        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        let address = match stack
            .dns_query("broker.hivemq.com", DnsQueryType::A)
            .await
            .map(|a| a[0])
        {
            Ok(address) => address,
            Err(e) => {
                println!("DNS lookup error: {e:?}");
                continue;
            }
        };

        let remote_endpoint = (address, 1883);
        println!("connecting...");
        let connection = socket.connect(remote_endpoint).await;
        if let Err(e) = connection {
            println!("connect error: {:?}", e);
            continue;
        }
        println!("connected!");

        let mut config = ClientConfig::new(
            rust_mqtt::client::client_config::MqttVersion::MQTTv5,
            CountingRng(20000),
        );
        config.add_max_subscribe_qos(rust_mqtt::packet::v5::publish_packet::QualityOfService::QoS1);
        config.add_client_id("clientId-8rhWgBODCl");
        config.max_packet_size = 100;
        let mut recv_buffer = [0; 80];
        let mut write_buffer = [0; 80];

        let mut client =
            MqttClient::<_, 5, _>::new(socket, &mut write_buffer, 80, &mut recv_buffer, 80, config);

        match client.connect_to_broker().await {
            Ok(()) => {}
            Err(mqtt_error) => match mqtt_error {
                ReasonCode::NetworkError => {
                    println!("MQTT Network Error");
                    continue;
                }
                _ => {
                    println!("Other MQTT Error: {:?}", mqtt_error);
                    continue;
                }
            },
        }

        let mut bmp = Bmp180::new(i2c0, sleep).await;
        loop {
            bmp.measure().await;
            let temperature = bmp.get_temperature();
            println!("Current temperature: {}", temperature);

            // Convert temperature into String
            let mut temperature_string: String<32> = String::new();
            write!(temperature_string, "{:.2}", temperature).expect("write! failed!");

            match client
                .send_message(
                    "temperature/1",
                    temperature_string.as_bytes(),
                    rust_mqtt::packet::v5::publish_packet::QualityOfService::QoS1,
                    true,
                )
                .await
            {
                Ok(()) => {}
                Err(mqtt_error) => match mqtt_error {
                    ReasonCode::NetworkError => {
                        println!("MQTT Network Error");
                        continue;
                    }
                    _ => {
                        println!("Other MQTT Error: {:?}", mqtt_error);
                        continue;
                    }
                },
            }
            Timer::after(Duration::from_millis(3000)).await;
        }
    }
}

pub async fn sleep(millis: u32) {
    Timer::after(Duration::from_millis(millis as u64)).await;
}
