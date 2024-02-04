use std::path::Path;
use std::io::Write;

use clap::Parser;
use serde::ser;
use serde::Deserialize;
use tokio::fs;
use tokio::join;
use log::{debug, error, info, log_enabled, warn};
use libsystemd::daemon;

use beelay::{core::{RunMode, BeelayCore}, service::BeelayService};


#[derive(Deserialize, Clone)]
struct MqttBroker {
    host: Option<String>,
    port: Option<u16>,
    topic: Option<String>
}

#[derive(Deserialize, Clone)]
struct Service {
    bind_address: Option<String>,
    port: Option<u16>,
    switches: Option<String>,
    cache_dir: Option<String>
}

#[derive(Deserialize, Clone)]
struct Config {
    mqttbroker: Option<MqttBroker>,
    service: Option<Service>
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Path to configuration TOML file.
    #[clap(short, long, default_value = "/etc/beelay/beelay.toml")]
    config: String,

    /// Use syslog.
   #[clap(long, action)]
   syslog: bool,

    /// Simulate for test
   #[clap(long, action)]
   simulate: bool
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct ConfigModeArgs {
    // Path to output configuration TOML file.
    #[clap(short, long)]
    output: Option<String>
}

const DEFAULT_CONFIG: &str = "
[service]
bind_address = \"127.0.0.1\"
port = 9999
#switches = \"example_switch1,example_switch2\"
switches = \"example_switch1,example_switch2\"
#cache_dir = \"/run/beelay\"
cache_dir = \"test/run/\"

[mqttbroker]
host = \"localhost\"
port = 1883
topic = \"zigbee2mqtt\"
";

#[tokio::main]
async fn main() {
    let exe = std::env::current_exe()
        .expect("Failed to get current exe from env")
        .file_name()
        .expect("Failed to derive exe base name")
        .to_owned();

    if exe == "beelay" {
        run_beelay().await;
    }
    else if exe == "beelay-config" {
        run_beelay_config().await;
    }
    else {
        panic!("Invalid exe name: {}", exe.to_string_lossy());
    }
}

async fn run_beelay() {
    let args = Args::parse();
    init_logging(args.syslog);

    let config: Config = toml::from_str(DEFAULT_CONFIG)
        .expect("Failed to parse default config; something is very wrong...");

    let config = apply_external_config(&config, &args.config).await;

    let service_config = config.service.unwrap();
    let bind_address = service_config.bind_address.unwrap();
    let bind_port = service_config.port.unwrap();
    let switches_str = service_config.switches.unwrap();
    let cache_dir = service_config.cache_dir.unwrap();

    let broker_config = config.mqttbroker.unwrap();
    let broker_host = broker_config.host.unwrap();
    let broker_port = broker_config.port.unwrap();
    let base_topic = broker_config.topic.unwrap();

    let run_mode;
    if args.simulate {
        run_mode = RunMode::Simulate;
    }
    else {
        run_mode = RunMode::MqttLink { host: broker_host, port: broker_port, base_topic: base_topic };
    }

    let mut switches: Vec<String> = Vec::new();
    for switch in switches_str.split(',') {
        switches.push(switch.to_string())
    }
    let switches = switches;

    let core = BeelayCore::new(&switches, &cache_dir, run_mode);
    let service = BeelayService::new(core, &bind_address, &bind_port);
    // if let Err(err) = service.run_service().await {
    //     panic!("Beelay service main loop crashed: {}", err);
    // }
    let (service_res, core_res) = join!(service.run_service(), service.run_core());
    if let Err(err) = service_res {
        error!("Service failed: {}", err);
    }
    if let Err(err) = core_res {
        error!("Core failed: {}", err);
    }
}

async fn run_beelay_config() {
    let args = ConfigModeArgs::parse();
    if let Some(out_file) = args.output {
        fs::write(out_file, DEFAULT_CONFIG).await
            .expect("Failed to write output configuration file");
    }
    else {
        print!("{}", DEFAULT_CONFIG);
    }
}

fn init_logging(use_syslog: bool) {
    let mut log_builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"));

    if use_syslog {
        log_builder.format(|buffer, record| {
            writeln!(buffer, "<{}>{}", record.level() as u8 + 2 , record.args())
        });
    }
    log_builder.init();
}

async fn apply_external_config(config: &Config, external_config_path: &str) -> Config {
    let new_config;
    if Path::new(external_config_path).exists() {
        let loaded_config: Config = toml::from_str(fs::read_to_string(external_config_path).await
            .expect("Failed to load config TOML").as_str())
            .expect("Failed to parse config TOML");

        let mut service = config.service.clone().unwrap();
        if let Some(loaded_service) = loaded_config.service {
            if let Some(bind_address) = loaded_service.bind_address {
                service.bind_address = Some(bind_address);
            }

            if let Some(port) = loaded_service.port {
                service.port = Some(port);
            }

            if let Some(switches) = loaded_service.switches {
                service.switches = Some(switches);
            }

            if let Some(cache_dir) = loaded_service.cache_dir {
                service.cache_dir = Some(cache_dir);
            }
        }

        let mut mqttbroker = config.mqttbroker.clone().unwrap();
        if let Some(loaded_mqttbroker) = loaded_config.mqttbroker {
            if let Some(host) = loaded_mqttbroker.host {
                mqttbroker.host = Some(host);
            }

            if let Some(port) = loaded_mqttbroker.port {
                mqttbroker.port = Some(port);
            }

            if let Some(topic) = loaded_mqttbroker.topic {
                mqttbroker.topic = Some(topic);
            }
        }

        new_config = Config{ service: Some(service), mqttbroker: Some(mqttbroker) };
    }
    else {
        warn!("Configuration file, {}, does not exist; using default configuration", external_config_path);
        new_config = config.clone();
    }

    new_config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_parse() {
        let config: Config = toml::from_str(DEFAULT_CONFIG)
            .expect("Failed to parse config");

        let service_config = config.service.unwrap();
        assert!(service_config.bind_address.unwrap() == "127.0.0.1");
        assert!(service_config.port.unwrap() == 9999);
        assert!(service_config.switches.is_none());
        assert!(service_config.cache_dir.unwrap() == "/run/beelay");

        let broker_config = config.mqttbroker.unwrap();
        assert!(broker_config.host.unwrap() == "localhost");
        assert!(broker_config.port.unwrap() == 1883);
        assert!(broker_config.topic.unwrap() == "zigbee2mqtt")
    }
}