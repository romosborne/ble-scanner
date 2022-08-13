// See the "macOS permissions note" in README.md before running this on macOS
// Big Sur or later.

use btleplug::api::{Central, CentralEvent, Manager as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use clap::Parser;
use futures::stream::StreamExt;
use paho_mqtt as mqtt;
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;

#[macro_use]
extern crate log;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, value_parser)]
    broker: String,

    #[clap(
        short,
        long,
        help = "mac address filter (comma separated)",
        value_parser
    )]
    filter: Option<String>,
}

#[derive(Serialize)]
struct SensorData {
    mac_address: String,
    temperature: f32,
    humidity: f32,
    battery_level: u8,
    battery_voltage: f32,
    counter: u8,
}

async fn get_central(manager: &Manager) -> Adapter {
    let adapters = manager.adapters().await.unwrap();
    adapters.into_iter().nth(0).unwrap()
}

fn parse_the_stuff(value: Vec<u8>) -> SensorData {
    /*
    All data little endian
    uint8_t     MAC[6]; // [0] - lo, .. [5] - hi digits
    int16_t     temperature;    // x 0.01 degree     [6,7]
    uint16_t    humidity;       // x 0.01 %          [8,9]
    uint16_t    battery_mv;     // mV                [10,11]
    uint8_t     battery_level;  // 0..100 %          [12]
    uint8_t     counter;        // measurement count [13]
    uint8_t     flags;  [14]
    */
    let mac = format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        value[5], value[4], value[3], value[2], value[1], value[0]
    );
    let temp = f32::from(u16::from(value[6]) | (u16::from(value[7]) << 8)) / 100.0;
    let hum = f32::from(u16::from(value[8]) | (u16::from(value[9]) << 8)) / 100.0;
    let battery_v = f32::from(u16::from(value[10]) | (u16::from(value[11]) << 8)) / 1000.0;
    let battery_level = value[12];
    let counter = value[13];

    SensorData {
        mac_address: mac,
        temperature: temp,
        humidity: hum,
        battery_level: battery_level,
        battery_voltage: battery_v,
        counter: counter,
    }
}

async fn publish(client: &mqtt::AsyncClient, sd: SensorData) -> Result<(), Box<dyn Error>> {
    info!("Publishing: {} for {}", sd.temperature, sd.mac_address);
    let json = serde_json::to_string(&sd)?;
    let topic = format!("home/sensor/mac/{}/info", sd.mac_address);
    let message = mqtt::Message::new(topic, json, mqtt::QOS_1);
    client.publish(message).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let args = Args::parse();

    info!("Connecting to broker: {}", args.broker);

    let filters: Option<Vec<String>> = match args.filter {
        Some(filter_string) => Some(
            filter_string
                .split(',')
                .map(|s| s.to_string())
                .collect::<Vec<String>>(),
        ),
        None => None,
    };

    match filters {
        Some(ref fs) => {
            info!("You supplied a filter:");
            for f in fs {
                info!(" - {}", f);
            }
        }
        None => info!("No filter supplied"),
    }

    let mut counters = HashMap::new();

    let mqtt_client = mqtt::AsyncClient::new(args.broker).unwrap_or_else(|err| {
        error!("Error creating the client: {}", err);
        panic!();
    });

    let mqtt_conn_opt = mqtt::ConnectOptionsBuilder::new()
        .keep_alive_interval(Duration::from_secs(20))
        .automatic_reconnect(Duration::from_secs(2), Duration::from_secs(500))
        .finalize();

    mqtt_client.connect(mqtt_conn_opt).await?;
    let message = mqtt::Message::new("test", "Greetings", mqtt::QOS_1);
    mqtt_client.publish(message).await?;

    publish(
        &mqtt_client,
        SensorData {
            mac_address: "mac".to_string(),
            temperature: 12.34,
            humidity: 69.0,
            battery_level: 98,
            battery_voltage: 2.8,
            counter: 1,
        },
    )
    .await?;

    let manager = Manager::new().await?;

    // get the first bluetooth adapter
    // connect to the adapter
    let central = get_central(&manager).await;

    // Each adapter has an event stream, we fetch via events(),
    // simplifying the type, this will return what is essentially a
    // Future<Result<Stream<Item=CentralEvent>>>.
    let mut events = central.events().await?;

    // start scanning for devices
    central.start_scan(ScanFilter::default()).await?;

    // Print based on whatever the event receiver outputs. Note that the event
    // receiver blocks, so in a real program, this should be run in its own
    // thread (not task, as this library does not yet use async channels).
    while let Some(event) = events.next().await {
        if let CentralEvent::ServiceDataAdvertisement {
            id: _,
            service_data,
        } = event
        {
            for (key, value) in service_data.into_iter() {
                let magic = key.as_bytes().windows(2).position(|s| s == [0x18, 0x1A]);
                if let Some(_) = magic {
                    // parse
                    let sensor_data = parse_the_stuff(value);

                    // filter
                    if let Some(ref fs) = filters {
                        if !fs.contains(&sensor_data.mac_address) {
                            info!("Filtering out: {}", &sensor_data.mac_address);
                            continue;
                        }
                    }

                    // check if new
                    let prev =
                        counters.insert(sensor_data.mac_address.clone(), sensor_data.counter);
                    if let Some(x) = prev {
                        if sensor_data.counter == x {
                            info!(
                                "Skipping repeated measurement for {}",
                                &sensor_data.mac_address
                            );
                            continue;
                        }
                    }

                    publish(&mqtt_client, sensor_data).await?;
                }
            }
        }
    }
    Ok(())
}
