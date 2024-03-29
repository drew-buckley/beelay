use std::fmt::write;
use std::ptr::slice_from_raw_parts_mut;
use std::{convert::Infallible, net::SocketAddr};
use std::io::Write;
use std::result::Result;
use std::sync::{Arc, Mutex};
use std::collections::{BTreeMap, HashMap};
use std::borrow::Cow;
use std::sync::mpsc::{sync_channel, SyncSender, Receiver};
use std::thread::{self, sleep};
use std::{str, time};
use std::path::Path;
use std::fs::{self, File};
use log::{debug, error, info, log_enabled, warn};
use clap::Parser;
use hyper::{Body, Request, Response, Server, Uri};
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use http::Method;
use rumqttc::{MqttOptions, Client, AsyncClient, QoS};
use rumqttc::Event::Incoming;
use rumqttc::Packet::Publish;
use serde_json;
use serde::{Deserialize};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Comma delimited list of named switches to whitelist.
    #[clap(short, long, default_value = "/etc/beelay/beelay.toml")]
    config: String,

    // IP address which to bind the server to.
    #[clap(short, long)]
    address: Option<String>,

    // Listening port for the server.
    #[clap(short, long)]
    port: Option<String>,

    // Comma delimited list of named switches to whitelist.
    #[clap(short, long)]
    switches: Option<String>,

    /// Use syslog.
   #[clap(long, action)]
   syslog: bool,

    /// Simulate for test
   #[clap(long, action)]
   simulate: bool
}

#[derive(Deserialize, Clone)]
struct Config {
    address: Option<String>,
    port: Option<String>,
    switches: Option<String>
}

enum OperationStatus {
    Succeeded,
    Failed(String)
}

const RUN_DIR: &str = "/run/beelay";

#[tokio::main]
async fn main() {
    let args = Args::parse();

    init_logging(args.syslog);

    let default_config = Config {
        address: Some("0.0.0.0".to_string()),
        port: Some("9999".to_string()),
        switches: None
    };

    let mut config: Config;
    if Path::new(args.config.as_str()).exists() {
        config = toml::from_str(fs::read_to_string(args.config)
            .expect("Failed to load config TOML.").as_str())
            .expect("Failed to parse config TOML.");
    }
    else {
        config = default_config.clone();
    }

    if args.address.is_some() {
        config.address = args.address;
    }

    if args.port.is_some() {
        config.port = args.port;
    }

    if args.switches.is_some() {
        config.switches = args.switches;
    }

    let switches_str = config.switches.unwrap();
    let address = config.address.unwrap();
    let port = config.port.unwrap();

    let mut switches: Vec<String> = Vec::new();
    for switch in switches_str.split(',') {
        switches.push(switch.to_string())
    }

    let switches = switches;
    let addr: SocketAddr = (address + ":" + &port)
        .parse()
        .expect("Unable to parse socket address.");

    let mqtt_send: SyncSender<String>;
    let http_receive: Receiver<String>;

    (mqtt_send, http_receive) = sync_channel(0);

    let http_send: SyncSender<String>;
    let mqtt_receive: Receiver<String>;

    (http_send, mqtt_receive) = sync_channel(0);

    let mqtt_send = Arc::new(Mutex::new(mqtt_send));
    let mqtt_receive = Arc::new(Mutex::new(mqtt_receive));
    let http_send = Arc::new(Mutex::new(http_send));

    let run_dir = Path::new(RUN_DIR);
    if !run_dir.exists() {
        std::fs::create_dir_all(&run_dir).unwrap();
    }

    let run_dir_lock: Arc<Mutex<String>> = Arc::new(Mutex::new(RUN_DIR.to_string()));
    let run_dir_lock_clone: Arc<Mutex<String>> = Arc::clone(&run_dir_lock);
    let simulate = args.simulate;
    tokio::spawn(async move {
        if let Err(e) = perform_mqtt_client_service(http_receive, http_send, run_dir_lock_clone, simulate).await {
            error!("Service error: {}", e);
        }
    });

    let switches_clone = switches.clone();
    let run_dir_lock_clone: Arc<Mutex<String>> = Arc::clone(&run_dir_lock);
    if !args.simulate {
        thread::spawn(move || {
            
            if let Err(err) = listen_for_state(switches_clone, run_dir_lock_clone) {
                error!("{}", err);
            }
        });
    }

    let run_dir_lock_clone: Arc<Mutex<String>> = Arc::clone(&run_dir_lock);
    if let Err(e) = perform_http_service(&addr, switches, mqtt_send, mqtt_receive, run_dir_lock_clone).await
    {
        error!("Service error: {}", e);
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

async fn perform_http_service(addr: &SocketAddr, switches: Vec<String>, 
                              mqtt_send: Arc<Mutex<SyncSender<String>>>, 
                              mqtt_receive: Arc<Mutex<Receiver<String>>>, 
                              run_dir_lock: Arc<Mutex<String>>) -> Result<(), String> {
    info!("Starting beelay HTTP service; listening on {}", addr.to_string());
    let switches = Arc::new(switches);
    let make_service = make_service_fn(move |conn: &AddrStream| {
        let switches = switches.clone();
        let addr = conn.remote_addr();
        let mqtt_send = Arc::clone(&mqtt_send);
        let mqtt_receive = Arc::clone(&mqtt_receive);
        let run_dir_lock = Arc::clone(&run_dir_lock);
        let service = service_fn(move |req| {
            handle(switches.clone(), addr, req, Arc::clone(&mqtt_send), Arc::clone(&mqtt_receive), Arc::clone(&run_dir_lock))
        });

        async move { Ok::<_, Infallible>(service) }
    });

    let server = Server::bind(&addr).serve(make_service);

    if let Err(e) = server.await 
    {
        return Err(e.to_string());
    }

    Ok(())
}

async fn handle(switches: Arc<Vec<String>>, 
                _addr: SocketAddr, 
                req: Request<Body>, 
                mqtt_send: Arc<Mutex<SyncSender<String>>>, 
                mqtt_receive: Arc<Mutex<Receiver<String>>>, 
                run_dir_lock: Arc<Mutex<String>>) -> Result<Response<Body>, Infallible> {
    let mut status = OperationStatus::Succeeded; 
    let mut switch_name: Option<Cow<str>> = None;
    let mut new_state: Option<Cow<str>> = None;
    let mut delay: Option<u64> = None;
    match req.uri().query() {
        Some(query_str) => {
           
            for query_pair in form_urlencoded::parse(query_str.as_bytes()) {
                let key = query_pair.0;
                let value = query_pair.1;

                match key.as_ref() {
                    "switch" => {
                        if switches.contains(&value.to_string()) {
                            switch_name = Some(value)
                        }
                        else {
                            status = OperationStatus::Failed("Unknown switch name.".to_string());
                        }
                    },
                    "state" => {
                        if vec!["on", "off"].contains(&value.as_ref().to_ascii_lowercase().as_str()) {
                            new_state = Some(value)
                        }
                        else {
                            status = OperationStatus::Failed("Unknown state.".to_string());
                        }
                    },
                    "delay" => {
                        if let Ok(delay_val) = value.parse::<u64>() {
                            delay = Some(delay_val);
                        }
                        else {
                            status = OperationStatus::Failed("Invalid delay value.".to_string());
                        }
                    }
                    _  => ()
                };
            }
        }
        None => {
            status = OperationStatus::Failed("No query parameters provided.".to_string());
        }
    };

    let mut fetched_state_value: Option<String> = None;

    match status {
        OperationStatus::Succeeded =>  {
            let method = req.method().clone();
            match method {
                Method::GET => {
                    match fetch_switch_state(switch_name.as_ref().unwrap(), mqtt_send.as_ref(), mqtt_receive.as_ref()) {
                        Ok(state) => fetched_state_value = Some(state),
                        Err(reason) => status = OperationStatus::Failed(reason)
                    }
                }
                Method::POST => {
                    match &new_state {
                        Some(new_state) => {
                            match set_switch_state(switch_name.as_ref().unwrap(), &new_state, mqtt_send.as_ref(), run_dir_lock, delay) {
                                Ok(()) => (),
                                Err(reason) => status = OperationStatus::Failed(reason)
                            }
                        },
                        None => status = OperationStatus::Failed("State value not specified.".to_string())
                    }
                    
                },
                _ => status = OperationStatus::Failed("Unsupport HTTP operation.".to_string())
            };
        }
        _ => ()
    }

    let json_respose = match generate_json_response(&status, fetched_state_value) {
        Ok(json) => json,
        Err(e) => format!("{{ \"Error\": \"{}\" }}", e.to_string())
    };

    Ok(Response::new(Body::from(json_respose)))
}

fn generate_json_response(op_status: &OperationStatus, 
                          switch_state: Option<String>) -> Result<String, String> {
    let mut json_staging_map: BTreeMap<String, String> = BTreeMap::new();
    
    match op_status {
        OperationStatus::Succeeded => {
            json_staging_map.insert("status".to_string(), "success".to_string());
            match switch_state {
                Some(state) => {
                    json_staging_map.insert("switch_state".to_string(), state.clone());
                }
                None => ()
            }
        },
        OperationStatus::Failed(reason) => {
            json_staging_map.insert("status".to_string(), "error".to_string());
            json_staging_map.insert("error_msg".to_string(), reason.clone());
        }
    }

    let json_respose = match serde_json::to_string(&json_staging_map) {
        Ok(json) => json,
        Err(e) => return Err(e.to_string())
    };

    Ok(json_respose)
}

fn fetch_switch_state(switch_name: &str, 
                      mqtt_send: &Mutex<SyncSender<String>>, 
                      mqtt_receive: &Mutex<Receiver<String>>) -> Result<String, String> {
    info!("Fetching state for {}", switch_name);

    {
        let mut guard = match mqtt_send.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner()
        };
        
        guard.send(format!("get,{}", switch_name))
            .expect("Channel is down!");
    }

    let mut state: String = "".to_string();    
    {
        let mut guard = match mqtt_receive.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner()
        };
        
        state = guard.recv().unwrap();
    }

    Ok(state)
}

fn set_switch_state(switch_name: &str,
                    new_state: &str, 
                    mqtt_send: &Mutex<SyncSender<String>>, 
                    run_dir_lock: Arc<Mutex<String>>, 
                    delay: Option<u64>) -> Result<(), String> {
    info!("Setting state for {} to {}", switch_name, new_state);
    let mut guard = match mqtt_send.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner()
    };

    let delay_val: u64;
    if let Some(delay) = delay {
        delay_val = delay;
    }
    else {
        delay_val = 0;
    }

    guard.send(format!("set,{},{},{}", switch_name, new_state, delay_val))
        .expect("Channel is down!");

    
    Ok(())
}

async fn perform_mqtt_client_service(http_receive: Receiver<String>, 
                                     http_send: Arc<Mutex<SyncSender<String>>>, 
                                     run_dir_lock: Arc<Mutex<String>>,
                                     simulate: bool) -> Result<(), String> {
    let mut should_run = Mutex::new(true);
    loop {
        let should_run_next_loop: bool;
        match should_run.lock() {
            Ok(guard) => should_run_next_loop = *guard,
            Err(_) => should_run_next_loop = false
        };

        if !should_run_next_loop {
            break;
        }

        let http_msg = http_receive.recv().unwrap();
        info!("MQTT service got message: {}", http_msg);
        let mut command: Option<String> = None;
        let mut switch_name: Option<String> = None;
        let mut new_state: Option<String> = None;
        let mut delay: Option<u64> = None;
        for token in http_msg.split(",") {
            if command.is_none() {
                command = Some(token.to_string());
            }
            else if switch_name.is_none() {
                switch_name = Some(token.to_string());
            }
            else if new_state.is_none() {
                new_state = Some(token.to_string());
            }
            else {
                delay = Some(token.parse::<u64>().unwrap());
            }
        }
        let command = command.unwrap();
        let http_send = Arc::clone(&http_send);
        let run_dir_lock = Arc::clone(&run_dir_lock);
        match command.as_str() {
            _ => {
                thread::spawn(move || {
                    if let Some(delay) = delay {
                        info!("Delaying MQTT transaction by {} seconds", delay);
                        thread::sleep(time::Duration::from_secs(delay));
                    }
                    if let Err(err) = perform_mqtt_transaction(
                            switch_name.unwrap().as_str(), 
                            &new_state, 
                            Arc::clone(&http_send), 
                            Arc::clone(&run_dir_lock), 
                            simulate) {
                        error!("{}", err);
                    }
                });
            }
        }
    }

    Ok(())
}

const MQTT_HOST: &str = "localhost";
const MQTT_PORT: u16 = 1883;
const ZIGBEE2MQTT_TOPIC: &str = "zigbee2mqtt";

fn perform_mqtt_transaction(switch_name: &str, 
                            new_state: &Option<String>, 
                            http_send: Arc<Mutex<SyncSender<String>>>, 
                            run_dir_lock: Arc<Mutex<String>>,
                            simulate: bool) -> Result<(), String> {
    {
        let mut guard = match run_dir_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner()
        };

        let run_dir_str = guard.to_string();
        let run_dir = Path::new(run_dir_str.as_str());
        let state_file_buffer = run_dir.join(switch_name);
        let state_file = state_file_buffer.as_path();
        match new_state {
            Some(new_state) => {
                
                let mut state_file_out = File::create(state_file).unwrap();
                write!(state_file_out, "{}", new_state);
            },
            None => {
                let retrieved_state = fs::read_to_string(state_file);
                if let Ok(guard) = http_send.as_ref().lock() {
                    if let Ok(state) = retrieved_state {
                        guard.send(state);
                    }
                    else {
                        guard.send("OFF".to_string());
                    }
                }
                return Ok(())
            }
        }
    }
    
    if simulate {
        info!("simulate is true; skipping MQTT transaction");
        return Ok(())
    }

    info!("Connecting to local mqtt service at {}:{}", MQTT_HOST, MQTT_PORT);
    let mut mqttoptions = MqttOptions::new("beelay-service", MQTT_HOST, MQTT_PORT);
    let (mut client, mut connection) = Client::new(mqttoptions, 10);
    client.subscribe(format!("zigbee2mqtt/{}", switch_name), QoS::AtMostOnce).unwrap();

    match new_state {
        Some(new_state) => {
            let topic = format!("zigbee2mqtt/{}/set", switch_name);
            info!("Sending {} ---> {}", topic, new_state);
            if let Err(e) = client.publish(topic, QoS::AtLeastOnce, false, new_state.as_str()) {
                error!("{}", e);
            }
        }
        None => {}
    }

    let new_state = new_state.clone();
    thread::spawn(move || {
        for (i, notification) in connection.iter().enumerate() {
            let notification_str = format!("Notification = {:?}", notification);
            let event = notification.unwrap();
            let mut state: Option<String> = None;
            match event {
                Incoming(incoming) => {
                    match incoming {
                        Publish(publish) => {
                            let payload: serde_json::Value = serde_json::from_str(str::from_utf8(&publish.payload).unwrap()).unwrap();
                            let payload: serde_json::Map<String, serde_json::Value> = payload.as_object().unwrap().clone();
                            if let Some(state_value) = payload.get("state") {
                                state = Some(state_value.as_str().unwrap().to_string());
                            }
                        }
                        _ => ()
                    }
                },
                _ => ()
            }

            if let Some(state) = state {
                if new_state.is_none() {
                    if let Ok(guard) = http_send.as_ref().lock() {
                        guard.send(state);
                    }
                }

                break;
            }
        }
    });

    Ok(())
}

fn listen_for_state(switches: Vec<String>, run_dir_lock: Arc<Mutex<String>>)  -> Result<(), String> {
    while true {
        info!("Listening to local mqtt service at {}:{}", MQTT_HOST, MQTT_PORT);
        let mut mqttoptions = MqttOptions::new("beelay-service", MQTT_HOST, MQTT_PORT);
        let (mut client, mut connection) = Client::new(mqttoptions, 10);

        for switch in &switches {
            client.subscribe(format!("{}/{}", ZIGBEE2MQTT_TOPIC, switch), QoS::AtMostOnce).unwrap();
        }

        for (i, notification) in connection.iter().enumerate() {
            let event = match notification {
                Ok(event) => event,
                Err(err) => {
                    error!("Error during listening: {}", err);
                    break;
                }
            };
            match event {
                Incoming(incoming) => {
                    match incoming {
                        Publish(publish) => {
                            let switch_name = &publish.topic[ZIGBEE2MQTT_TOPIC.len()+1..publish.topic.len()];
                            info!("Got packet for {}", switch_name);
                            let payload: serde_json::Value = serde_json::from_str(str::from_utf8(&publish.payload).unwrap()).unwrap();
                            let payload: serde_json::Map<String, serde_json::Value> = payload.as_object().unwrap().clone();
                            if let Some(state_value) = payload.get("state") {
                                let state = state_value.as_str().unwrap().to_string();
                                {
                                    let mut guard = match run_dir_lock.lock() {
                                        Ok(guard) => guard,
                                        Err(poisoned) => poisoned.into_inner()
                                    };

                                    let run_dir_str = guard.to_string();
                                    let run_dir = Path::new(run_dir_str.as_str());
                                    let state_file_buffer = run_dir.join(switch_name);
                                    let state_file = state_file_buffer.as_path();
                                    let mut state_file_out = File::create(state_file).unwrap();
                                    write!(state_file_out, "{}", state);
                                }
                            }
                        }
                        _ => ()
                    }
                },
                _ => ()
            }
        }
    }

    Ok(())
}
