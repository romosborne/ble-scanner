// See the "macOS permissions note" in README.md before running this on macOS
// Big Sur or later.

use btleplug::api::{Central, CentralEvent, Manager as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use clap::Parser;
use futures::stream::StreamExt;
use paho_mqtt as mqtt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::time::Duration;
use uuid::Uuid;

#[macro_use]
extern crate log;

#[derive(Deserialize)]
struct Config {
    broker: String,
    sensors: Vec<Sensor>,
}

#[derive(Deserialize)]
struct Sensor {
    mac: String,
    name: String,
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, value_parser)]
    config: String,
}

#[derive(Serialize, Clone)]
struct DeviceDiscoveryPayload {
    manufacturer: String,
    name: String,
    identifiers: String,
}

#[derive(Serialize)]
struct SensorDiscoveryPayload {
    name: String,
    availability_topic: String,
    device_class: String,
    state_topic: String,
    unit_of_measurement: String,
    value_template: String,
    unique_id: String,
    device: DeviceDiscoveryPayload,
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

async fn publish(
    client: &mqtt::AsyncClient,
    availability_topic: &String,
    sd: SensorData,
) -> Result<(), Box<dyn Error>> {
    info!("Publishing: {} for {}", sd.temperature, sd.mac_address);

    client.publish(mqtt::Message::new(
        availability_topic,
        "online",
        mqtt::QOS_1,
    ));

    let json = serde_json::to_string(&sd)?;
    let topic = format!("home/sensor/mac/{}/info", sd.mac_address);
    let message = mqtt::Message::new_retained(topic, json, mqtt::QOS_1);
    client.publish(message).await?;
    Ok(())
}

async fn setup_autodiscovery(
    sensors: &Vec<Sensor>,
    availability_topic: &String,
    mqtt: &mqtt::AsyncClient,
) -> Result<(), Box<dyn Error>> {
    for s in sensors {
        let ident = s.mac.replace(":", "");

        let device = DeviceDiscoveryPayload {
            manufacturer: "Nozzytronics".to_string(),
            name: format!("NozzyEnviro-{}", s.name),
            identifiers: ident.to_string(),
        };

        send(
            mqtt,
            SensorDiscoveryPayload {
                name: format!("{}-temp", s.name),
                availability_topic: availability_topic,
                device_class: "temperature".to_string(),
                state_topic: format!("home/sensor/mac/{}/info", s.mac),
                unit_of_measurement: "°C".to_string(),
                value_template: "{{value_json.temperature}}".to_string(),
                unique_id: format!("{}-temp", s.name),
                device: device.clone(),
            },
        )
        .await?;

        send(
            mqtt,
            SensorDiscoveryPayload {
                name: format!("{}-humidity", s.name),
                availability_topic: availability_topic,
                device_class: "humidity".to_string(),
                state_topic: format!("home/sensor/mac/{}/info", s.mac),
                unit_of_measurement: "%".to_string(),
                value_template: "{{value_json.humidity}}".to_string(),
                unique_id: format!("{}-humidity", s.name),
                device: device.clone(),
            },
        )
        .await?;

        send(
            mqtt,
            SensorDiscoveryPayload {
                name: format!("{}-batteryvoltage", s.name),
                availability_topic: availability_topic,
                device_class: "voltage".to_string(),
                state_topic: format!("home/sensor/mac/{}/info", s.mac),
                unit_of_measurement: "V".to_string(),
                value_template: "{{value_json.battery_voltage}}".to_string(),
                unique_id: format!("{}-batteryvoltage", s.name),
                device: device.clone(),
            },
        )
        .await?;

        send(
            mqtt,
            SensorDiscoveryPayload {
                name: format!("{}-batterylevel", s.name),
                availability_topic: availability_topic,
                device_class: "battery".to_string(),
                state_topic: format!("home/sensor/mac/{}/info", s.mac),
                unit_of_measurement: "%".to_string(),
                value_template: "{{value_json.battery_level}}".to_string(),
                unique_id: format!("{}-batterylevel", s.name),
                device: device.clone(),
            },
        )
        .await?;
    }
    Ok(())
}

async fn send(
    mqtt: &mqtt::AsyncClient,
    payload: SensorDiscoveryPayload,
) -> Result<(), Box<dyn Error>> {
    let topic = format!("homeassistant/sensor/{}/config", payload.unique_id);
    let json = serde_json::to_string(&payload)?;

    let message = mqtt::Message::new(topic, json, mqtt::QOS_1);
    Ok(mqtt.publish(message).await?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let args = Args::parse();
    let config_data = fs::read_to_string(args.config).expect("Unable to read config file");

    let config: Config = serde_json::from_str(&config_data).expect("Unable to parse config");

    let uuid = Uuid::new_v4();
    let availability_topic = format!("home/sensor/uuid/{}/availability", uuid);

    info!("Connecting to broker: {}", config.broker);

    for sensor in &config.sensors {
        info!("Filtering to mac address: {}", sensor.mac);
        info!("Name for Home Assistant: {}", sensor.name);
    }

    let mut counters = HashMap::new();

    let mqtt_client = mqtt::AsyncClient::new(config.broker).unwrap_or_else(|err| {
        error!("Error creating the client: {}", err);
        panic!();
    });

    let mqtt_conn_opt = mqtt::ConnectOptionsBuilder::new()
        .keep_alive_interval(Duration::from_secs(20))
        .automatic_reconnect(Duration::from_secs(2), Duration::from_secs(500))
        .will_message(mqtt::Message::new(
            availability_topic,
            "offline",
            mqtt::QOS_1,
        ))
        .finalize();

    mqtt_client.connect(mqtt_conn_opt).await?;

    // Trigger autodiscovery
    setup_autodiscovery(&config.sensors, &availability_topic, &mqtt_client).await?;

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
                    let x = config
                        .sensors
                        .iter()
                        .map(|s| s.mac.to_owned())
                        .collect::<Vec<String>>();

                    if !x.contains(&sensor_data.mac_address) {
                        info!("Filtering out: {}", &sensor_data.mac_address);
                        continue;
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

                    publish(&mqtt_client, &availability_topic, sensor_data).await?;
                }
            }
        }
    }
    Ok(())
}
