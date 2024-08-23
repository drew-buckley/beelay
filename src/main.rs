use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::io::Write;
use std::process::exit;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use std::sync::atomic::Ordering::Relaxed;

use beelay::core::build_core;
use beelay::core::BeelayCoreCtrl;
use beelay::mqtt_client::build_mqtt_client;
use beelay::mqtt_client::build_mqtt_simulation_client;
use beelay::mqtt_client::MqttClientCtrl;
use beelay::service::build_service;
use beelay::service::BeelayServiceCtrl;
use clap::Parser;
use indexmap::IndexMap;
use serde::Deserialize;
use tokio::fs;
use log::{error, info, warn};

use tokio::signal::unix::signal;
use tokio::signal::unix::SignalKind;


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
    service: Option<Service>,
    filters: Option<HashMap<String, String>>,
    pretty_names: Option<IndexMap<String, String>>
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to configuration TOML file.
    #[clap(short, long, default_value = "/etc/beelay/beelay.toml")]
    config: String,

    /// Use syslog.
    #[clap(long, action)]
    syslog: bool,

    /// Notify systemd.
    #[clap(long, action)]
    sd_notify: bool,

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

const DEFAULT_CONFIG: &str = include_str!("../beelay-default.toml");
const RUN_EXE_CONTEXT: &str = "RUN";
const CONFIG_EXE_CONTEXT: &str = "CONFIG";

#[tokio::main]
async fn main() {
    let exe_context = match env::var("BEELAY_EXE_CONTEXT") {
        Ok(var) => var,
        Err(_) => {
            let exe = std::env::current_exe()
                .expect("Failed to get current exe from env")
                .file_name()
                .expect("Failed to derive exe base name")
                .to_owned();

            if exe == "beelay" {
                RUN_EXE_CONTEXT.to_string()
            }
            else if exe == "beelay-config" {
                CONFIG_EXE_CONTEXT.to_string()
            }
            else {
                panic!("Invalid exe name: {}", exe.to_string_lossy());
            }
        }
    };

    if exe_context == RUN_EXE_CONTEXT {
        run_beelay().await;
    }
    else if exe_context == CONFIG_EXE_CONTEXT {
        run_beelay_config().await;
    }
    else {
        panic!("Invalid execution context name: {}", exe_context);
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
    let cache_dir = service_config.cache_dir.unwrap();

    let broker_config = config.mqttbroker.unwrap();
    let broker_host = broker_config.host.unwrap();
    let broker_port = broker_config.port.unwrap();
    let base_topic = broker_config.topic.unwrap();

    let mut switches: Vec<String> = Vec::new();
    let switches_str = service_config.switches;
    if switches_str.is_some() {
        let switches_str = switches_str.unwrap();
        for switch in switches_str.split(',') {
            switches.push(switch.to_string())
        }
    }
    let switches = switches;

    let cache_dir_exists = fs::try_exists(&cache_dir).await
        .expect(format!("Failed to check cache directory existence: {}", cache_dir).as_str());
    if !cache_dir_exists {
        fs::create_dir(&cache_dir).await
            .expect(format!("Failed to create cache directory: {}", cache_dir).as_str());
    }

    let filters = HashMap::new();
    if let Some(filter_strs) = config.filters {
        for (filter_name, filter_list) in filter_strs {
            let mut switches: Vec<String> = Vec::new();
            for switch in filter_list.split(',') {
                switches.push(switch.to_string())
            }
            filters.insert(filter_name, switches);
        }
    }

    let (mqtt_ctrl, mqtt_task_running) = launch_mqtt_task(broker_host, broker_port, base_topic, args.simulate);
    let (core_ctrl, core_task_running) = launch_core_task(&switches, &cache_dir, mqtt_ctrl.clone());
    let (service_ctrl, service_task_running) = launch_service_task(core_ctrl.clone(), &switches, &bind_address, &bind_port, &filters, &config.pretty_names);

    let should_run = Arc::new(AtomicBool::new(true));
    launch_signal_monitor(&service_ctrl, &service_task_running, &core_ctrl, &core_task_running, &mqtt_ctrl, &mqtt_task_running, &should_run, args.sd_notify);
    
    if args.sd_notify {
        notify_systemd_ready();
    }

    while should_run.load(Relaxed) {
        if let Err(err) = mqtt_ctrl.ping().await {
            error!("MQTT ping failed: {}", err);
        }
        if let Err(err) = core_ctrl.ping().await {
            error!("Core ping failed: {}", err);
        }
        if let Err(err) = service_ctrl.ping().await {
            error!("Service ping failed: {}", err);
        }

        if args.sd_notify {
            pet_systemd_watchdog();
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    info!("Beelay done!")
}

fn launch_mqtt_task(broker_host: String, broker_port: u16, base_topic: String, simulate: bool) -> (MqttClientCtrl, Arc<AtomicBool>) {
    let mqtt_ctrl;
    let mqtt_task_running = Arc::new(AtomicBool::new(true));
    let mqtt_task_running_clone = Arc::clone(&mqtt_task_running);

    if simulate {
        warn!("Running in simulation mode! MQTT broker will be ignored");
        let mqtt_task_running = mqtt_task_running_clone;
        let mut mqtt_client;
        (mqtt_client, mqtt_ctrl) = build_mqtt_simulation_client(Duration::from_millis(1000), 64);
        tokio::spawn(async move {
            if let Err(err) = mqtt_client.run().await {
                error!("MQTT simulation client crashed: {}", err);
            }

            mqtt_task_running.store(false, Relaxed);
        });
    }
    else {
        let mqtt_task_running = mqtt_task_running_clone;
        let mut mqtt_client;
        (mqtt_client, mqtt_ctrl) = build_mqtt_client(broker_host, broker_port, base_topic, 64);
        tokio::spawn(async move {
            if let Err(err) = mqtt_client.run().await {
                error!("MQTT client crashed: {}", err);
            }

            mqtt_task_running.store(false, Relaxed);
        });
    }

    (mqtt_ctrl, mqtt_task_running)
}

fn launch_core_task(
    switch_names: &Vec<String>,
    switch_cache_dir: &str,
    mqtt_ctrl: MqttClientCtrl
) -> (BeelayCoreCtrl, Arc<AtomicBool>) {
    let (mut core, core_ctrl) = build_core(switch_names, switch_cache_dir, mqtt_ctrl.clone(), 64);
    let core_task_running = Arc::new(AtomicBool::new(true));
    let core_task_running_clone = Arc::clone(&core_task_running);
    tokio::spawn(async move {
        let core_task_running = core_task_running_clone;
        if let Err(err) = core.run().await {
            error!("Beelay core crashed: {}", err);
        }

        core_task_running.store(false, Relaxed);
    });

    (core_ctrl, core_task_running)
}

fn launch_service_task(
    core_ctrl: BeelayCoreCtrl,
    switches: &Vec<String>,
    address: &str,
    port: &u16,
    filter_map: &HashMap<String, Vec<String>>,
    pretty_map: &Option<IndexMap<String, String>>
) -> (BeelayServiceCtrl, Arc<AtomicBool>) {
    let (mut service, service_ctrl) = build_service(core_ctrl.clone(), &switches, address, port, filter_map, pretty_map, 64);
    let service_task_running = Arc::new(AtomicBool::new(true));
    let service_task_running_clone = Arc::clone(&service_task_running);
    tokio::spawn(async move {
        let service_task_running = service_task_running_clone;
        if let Err(err) = service.run().await {
            error!("Beelay service crashed: {}", err);
        }

        service_task_running.store(false, Relaxed);
    });

    (service_ctrl, service_task_running)
}

fn launch_signal_monitor(service_ctrl: &BeelayServiceCtrl,
                         service_task_running: &Arc<AtomicBool>,
                         core_ctrl: &BeelayCoreCtrl,
                         core_task_running: &Arc<AtomicBool>,
                         mqtt_ctrl: &MqttClientCtrl,
                         mqtt_task_running: &Arc<AtomicBool>,
                         should_run: &Arc<AtomicBool>,
                         sd_notify: bool) {
    let service_ctrl = service_ctrl.clone();
    let service_task_running = Arc::clone(service_task_running);
    let core_ctrl = core_ctrl.clone();
    let core_task_running = Arc::clone(core_task_running);
    let mqtt_client_ctrl = mqtt_ctrl.clone();
    let mqtt_task_running = Arc::clone(mqtt_task_running);
    let should_run = Arc::clone(should_run);

    tokio::spawn(async move {
        monitor_signals(service_ctrl, 
                        service_task_running, 
                        core_ctrl, 
                        core_task_running, 
                        mqtt_client_ctrl, 
                        mqtt_task_running, 
                        should_run,
                        sd_notify).await;
    });
}

async fn shutdown_beelay(service_ctrl: BeelayServiceCtrl,
                         service_task_running: Arc<AtomicBool>,
                         core_ctrl: BeelayCoreCtrl,
                         core_task_running: Arc<AtomicBool>,
                         mqtt_ctrl: MqttClientCtrl,
                         mqtt_task_running: Arc<AtomicBool>) {
    if let Err(err) = service_ctrl.stop().await {
        error!("Failed to stop service: {}", err);
    }

    if let Err(err) = core_ctrl.stop().await {
        error!("Failed to stop core: {}", err);
    }

    if let Err(err) = mqtt_ctrl.stop().await {
        error!("Failed to stop MQTT client: {}", err);
    }

    info!("Waiting for tasks to complete");
    let mut waiting_for_task = mqtt_task_running.load(Relaxed);
    while waiting_for_task {
        tokio::time::sleep(Duration::from_micros(250)).await;
        waiting_for_task = mqtt_task_running.load(Relaxed)
                         & core_task_running.load(Relaxed)
                         & service_task_running.load(Relaxed);
    }
}

async fn monitor_signals(service_ctrl: BeelayServiceCtrl,
                         service_task_running: Arc<AtomicBool>,
                         core_ctrl: BeelayCoreCtrl,
                         core_task_running: Arc<AtomicBool>,
                         mqtt_ctrl: MqttClientCtrl,
                         mqtt_task_running: Arc<AtomicBool>,
                         should_run: Arc<AtomicBool>,
                         sd_notify: bool) {
    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    let mut sigint = signal(SignalKind::interrupt()).unwrap();

    tokio::select! {
        _ = sigterm.recv() => info!("Receive shutdown signal"),
        _ = sigint.recv() => info!("Receive shutdown signal"),
    };

    if sd_notify {
        notify_systemd_stopping();
    }

    tokio::select! {
        _ = shutdown_beelay(service_ctrl, service_task_running, core_ctrl, core_task_running, mqtt_ctrl, mqtt_task_running) => info!("Tasks have finished"),
        _ = sigterm.recv() => {
            info!("Received additional shutdown signal; forcing");
            exit(-1);
        },
        _ = sigint.recv() => {
            info!("Received additional shutdown signal; forcing");
            exit(-1);
        },
    };

    should_run.store(false, Relaxed);
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

#[cfg(feature = "systemd")]
fn notify_systemd_ready() {
    let _ = libsystemd::daemon::notify(false, &[libsystemd::daemon::NotifyState::Ready]);
}

#[cfg(not(feature = "systemd"))]
fn notify_systemd_ready() {
    warn!("Beelay not built with systemd support; cannot notify ready!")
}

#[cfg(feature = "systemd")]
fn pet_systemd_watchdog() {
    let _ = libsystemd::daemon::notify(false, &[libsystemd::daemon::NotifyState::Watchdog]);
}

#[cfg(not(feature = "systemd"))]
fn pet_systemd_watchdog() {
    warn!("Beelay not built with systemd support; cannot pet watchdog!")
}

#[cfg(feature = "systemd")]
fn notify_systemd_stopping() {
    let _ = libsystemd::daemon::notify(false, &[libsystemd::daemon::NotifyState::Stopping]);
}

#[cfg(not(feature = "systemd"))]
fn notify_systemd_stopping() {
    warn!("Beelay not built with systemd support; cannot notify stopping!")
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