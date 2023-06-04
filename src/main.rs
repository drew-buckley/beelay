use std::ptr::slice_from_raw_parts_mut;
use std::{convert::Infallible, net::SocketAddr};
use std::io::Write;
use std::result::Result;
use std::sync::{Arc, Mutex};
use std::collections::BTreeMap;
use std::borrow::Cow;
use std::sync::mpsc::{sync_channel, SyncSender, Receiver};
use std::thread::{self, sleep};
use log::{debug, error, info, log_enabled, warn};
use clap::Parser;
use hyper::{Body, Request, Response, Server, Uri};
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use http::Method;
use rumqttc::{MqttOptions, Client, AsyncClient, QoS};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Comma delimited list of named switches to whitelist.
    #[clap(short, long, default_value = "/etc/beelay/beelay.toml")]
    config: String,

    // IP address which to bind the server to.
    #[clap(short, long, default_value = "0.0.0.0")]
    address: String,

    // Listening port for the server.
    #[clap(short, long, default_value = "9999")]
    port: String,

    // Comma delimited list of named switches to whitelist.
    #[clap(short, long, default_value = "")]
    switches: String,

    /// Use syslog.
   #[clap(long, action)]
   syslog: bool
}

enum OperationStatus {
    Succeeded,
    Failed(String)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    init_logging(args.syslog);

    let mut switches: Vec<String> = Vec::new();
    for switch in args.switches.split(',') {
        switches.push(switch.to_string())
    }
    
    let switches = switches;
    let addr: SocketAddr = (args.address + ":" + &args.port)
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

    tokio::spawn(async move {
        if let Err(e) = perform_mqtt_client_service(http_receive, http_send).await {
            error!("Service error: {}", e);
        }
    });

    if let Err(e) = perform_http_service(&addr, switches, mqtt_send, mqtt_receive).await
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

async fn perform_http_service(addr: &SocketAddr, switches: Vec<String>, mqtt_send: Arc<Mutex<SyncSender<String>>>, mqtt_receive: Arc<Mutex<Receiver<String>>>) -> Result<(), String> {
    info!("Starting beelay HTTP service; listening on {}", addr.to_string());
    let switches = Arc::new(switches);
    let make_service = make_service_fn(move |conn: &AddrStream| {
        let switches = switches.clone();
        let addr = conn.remote_addr();
        let mqtt_send = Arc::clone(&mqtt_send);
        let mqtt_receive = Arc::clone(&mqtt_receive);
        let service = service_fn(move |req| {
            handle(switches.clone(), addr, req, Arc::clone(&mqtt_send), Arc::clone(&mqtt_receive))
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

async fn handle(switches: Arc<Vec<String>>, _addr: SocketAddr, req: Request<Body>, mqtt_send: Arc<Mutex<SyncSender<String>>>, mqtt_receive: Arc<Mutex<Receiver<String>>>) -> Result<Response<Body>, Infallible> {
    let mut status = OperationStatus::Succeeded; 
    let mut switch_name: Option<Cow<str>> = None;
    let mut new_state: Option<Cow<str>> = None;
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
                    match fetch_switch_state(switch_name.as_ref().unwrap()) {
                        Ok(state) => fetched_state_value = Some(state),
                        Err(reason) => status = OperationStatus::Failed(reason)
                    }
                }
                Method::POST => {
                    match &new_state {
                        Some(new_state) => {
                            match set_switch_state(switch_name.as_ref().unwrap(), &new_state, mqtt_send.as_ref()) {
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

fn generate_json_response(op_status: &OperationStatus, switch_state: Option<String>) -> Result<String, String> {
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

fn fetch_switch_state(switch_name: &str) -> Result<String, String> {
    info!("Fetching state for {}", switch_name);
    Ok("ON".to_string())
}

fn set_switch_state(switch_name: &str, new_state: &str, mqtt_send: &Mutex<SyncSender<String>>) -> Result<(), String> {
    info!("Setting state for {} to {}", switch_name, new_state);
    let mut guard = match mqtt_send.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner()
    };

    guard.send(format!("set,{},{}", switch_name, new_state))
        .expect("Channel is down!");

    
    Ok(())
}

async fn perform_mqtt_client_service(http_receive: Receiver<String>, http_send: SyncSender<String>) -> Result<(), String> {
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
        for token in http_msg.split(",") {
            if command.is_none() {
                command = Some(token.to_string());
            }
            else if switch_name.is_none() {
                switch_name = Some(token.to_string());
            }
            else {
                new_state = Some(token.to_string());
            }
        }
        let command = command.unwrap();
        match command.as_str() {
            _ => {
                thread::spawn(move || {
                    if let Err(err) = perform_mqtt_transaction(switch_name.unwrap().as_str(), &new_state) {
                        error!("{}", err);
                    }
                });
            }
        }
    }

    Ok(())
}

fn perform_mqtt_transaction(switch_name: &str, new_state: &Option<String>)  -> Result<(), String> {
    const MQTT_HOST: &str = "localhost";
    const MQTT_PORT: u16 = 1883;

    info!("Connecting to local mqtt service at {}:{}", MQTT_HOST, MQTT_PORT);
    let mut mqttoptions = MqttOptions::new("beelay-service", MQTT_HOST, MQTT_PORT);
    let (mut client, mut connection) = Client::new(mqttoptions, 10);
    match new_state {
        Some(new_state) => {
            let topic = format!("zigbee2mqtt/{}/set", switch_name);
            info!("Sending {} ---> {}", topic, new_state);
            if let Err(e) = client.publish(topic, QoS::AtLeastOnce, false, new_state.as_str()) {
                error!("{}", e);
            }
        }
        None => {
            let topic = format!("zigbee2mqtt/{}/get", switch_name);
            info!("Sending {}", topic);
            if let Err(e) = client.publish(topic, QoS::AtLeastOnce, false, "") {
                error!("{}", e);
            }
        }
    }

    for (i, notification) in connection.iter().enumerate() {
        let notification_str = format!("Notification = {:?}", notification);
        if notification_str.contains("PubAck") {
            break;
        }
    }

    Ok(())
}

async fn _perform_mqtt_client_service(http_receive: Receiver<String>, http_send: SyncSender<String>) -> Result<(), String> {
    const MQTT_HOST: &str = "localhost";
    const MQTT_PORT: u16 = 1883;

    info!("Connecting to local mqtt service at {}:{}", MQTT_HOST, MQTT_PORT);
    let mut mqttoptions = MqttOptions::new("beelay-service", MQTT_HOST, MQTT_PORT);
    let (mut client, mut eventloop) = AsyncClient::new(mqttoptions, 50);

    info!("Subscribing to all zigbee2mqtt messages");
    client.subscribe("zigbee2mqtt/#", QoS::AtMostOnce).await.unwrap();

    let mut should_run = Mutex::new(true);
    
    tokio::spawn(async move {
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
            for token in http_msg.split(",") {
                if command.is_none() {
                    command = Some(token.to_string());
                }
                else if switch_name.is_none() {
                    switch_name = Some(token.to_string());
                }
                else {
                    new_state = Some(token.to_string());
                }
            }
            let command = command.unwrap();
            match command.as_str() {
                "set" => {
                    let switch_name = switch_name.unwrap();
                    let new_state = new_state.unwrap();
                    let topic = format!("zigbee2mqtt/{}/set", switch_name);
                    info!("Sending {} ---> {}", topic, new_state);
                    if let Err(e) = client.publish(topic, QoS::AtLeastOnce, false, new_state).await {
                        error!("{}", e);
                    }
                    info!("Sent!");
                },
                _ => panic!("Got unrecognized command {}", command)
            }
        }
    });

    loop {
        info!("Waiting for next notification");
        match eventloop.poll().await {
            Ok(notification) => {
                info!("Received = {:?}", notification);
            },
            Err(err) => {
                error!("{}", err);
            }
        }
        
    }

    Ok(())
}
