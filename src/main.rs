// See the "macOS permissions note" in README.md before running this on macOS
// Big Sur or later.

use btleplug::api::Peripheral;
use btleplug::api::{bleuuid::BleUuid, Central, CentralEvent, Manager as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use futures::stream::StreamExt;
use paho_mqtt as mqtt;
use std::collections::HashMap;
use std::error::Error;

#[macro_use]
extern crate log;

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

    println!(
        "{} - {}, Temp: {}, Hum: {}%, Bv: {}, Blev: {}%",
        counter, mac, temp, hum, battery_v, battery_level
    );

    SensorData {
        mac_address: mac,
        temperature: temp,
        humidity: hum,
        battery_level: battery_level,
        battery_voltage: battery_v,
        counter: counter,
    }
}

fn publish(sd: SensorData) {
    info!("Publishing: {} for {}", sd.temperature, sd.mac_address)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut counters = HashMap::new();

    let mqtt_client = mqtt::AsyncClient::new("tcp://192.168.1.20:1883").unwrap_or_else(|err| {
        error!("Error creating the client: {}", err);
        panic!();
    });

    mqtt_client.connect(None).await?;
    let message = mqtt::Message::new("test", "Greetings", mqtt::QOS_1);
    mqtt_client.publish(message).await?;
    mqtt_client.disconnect(None).await?;

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
        if let CentralEvent::ServiceDataAdvertisement { id, service_data } = event {
            // let p = central.peripheral(&id).await.unwrap();
            // println!("ServiceDataAdvertisement: {:?}, {:?}", p.address(), service_data);
            for (key, value) in service_data.into_iter() {
                let magic = key.as_bytes().windows(2).position(|s| s == [0x18, 0x1A]);
                if let Some(_) = magic {
                    // parse
                    let sensor_data = parse_the_stuff(value);

                    // check if new
                    let prev =
                        counters.insert(sensor_data.mac_address.clone(), sensor_data.counter);
                    if let Some(x) = prev {
                        if (sensor_data.counter == x) {
                            info!("Repeated measurement");
                            continue;
                        }
                    }

                    publish(sensor_data);
                }
            }
        }
    }
    Ok(())
}
