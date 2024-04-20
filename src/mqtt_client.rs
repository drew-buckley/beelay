use std::{collections::{HashMap, LinkedList}, error::Error, fmt, time::Duration};
use tokio::{self, time::Instant};
use tokio::sync::mpsc;
use rumqttc::{MqttOptions, AsyncClient, QoS, Event};
use log::{debug, error, info, trace, warn};

use crate::common::{build_message_link_transactor, str_to_switch_state, switch_state_to_str, MessageLink, MessageLinkTransactor, SwitchState};

#[derive(Debug)]
pub struct BeelayMqttClientError {
    message: String
}

impl Error for BeelayMqttClientError {}

impl BeelayMqttClientError {
    fn new(message: &str) -> BeelayMqttClientError {
        BeelayMqttClientError{ message: message.to_string() }
    }
}

impl fmt::Display for BeelayMqttClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "BeelayMqttClientError: {}", self.message)
    }
}

#[derive(Clone)]
enum Command {
    Ping,
    Set{ switch_name: String, state: SwitchState },
    Stop,
    Reset
}

#[derive(Clone)]
enum CommandResponse {
    Ack
}

impl CommandResponse {
    fn is_ack(&self) -> bool {
        match &self {
            CommandResponse::Ack => true
        }
    }
}

impl fmt::Display for CommandResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CommandResponse::Ack => {
                write!(f, "CommandResponse::Ack")
            }
        }
    }
}

pub fn build_mqtt_simulation_client(state_update_interval: Duration, msg_queue_cap: usize) -> (MqttClientSimulator, MqttClientCtrl) {
    let (mlt, rx) = build_message_link_transactor(msg_queue_cap);
    let (state_link_tx, state_link_rx) = mpsc::channel(32);

    let ctrl = MqttClientCtrl { 
        msg_link_transactor: mlt,
        state_link_tx: state_link_tx
    };

    let sim = MqttClientSimulator {
        state_update_interval,
        command_rx: rx,
        state_link_rx: state_link_rx,
        state_map: HashMap::new()
    };

    (sim, ctrl)
}

pub fn build_mqtt_client(switch_names: Vec<String>, host: String, port: u16, base_topic: String, msg_queue_cap: usize) -> (MqttClient, MqttClientCtrl) {
    let (mlt, rx) = build_message_link_transactor(msg_queue_cap);
    let (state_link_tx, state_link_rx) = mpsc::channel(32);

    let ctrl = MqttClientCtrl { 
        msg_link_transactor: mlt,
        state_link_tx: state_link_tx
    };

    let client = MqttClient {
        command_rx: rx,
        state_link_rx: state_link_rx,
        base_topic: base_topic,
        host: host,
        port: port,
        switch_names: switch_names
    };

    (client, ctrl)
}

pub struct MqttClientSimulator {
    state_update_interval: Duration,
    command_rx: mpsc::Receiver<MessageLink<Command, CommandResponse>>,
    state_link_rx: mpsc::Receiver<mpsc::Sender<(String, SwitchState)>>,
    state_map: HashMap<String, SwitchState>
}

impl MqttClientSimulator {
    pub async fn run(&mut self) -> Result<(), Box<dyn Error>> {
        let mut state_senders = LinkedList::new();
        let mut should_run = true;
        let mut instant = Instant::now();
        while should_run {
            match self.state_link_rx.try_recv() {
                Ok(tx) => {
                    state_senders.push_back(tx);
                },
                Err(err) => match err {
                    mpsc::error::TryRecvError::Empty => (),
                    mpsc::error::TryRecvError::Disconnected => {
                        return Err(
                            Box::new(
                                BeelayMqttClientError::new("State link channel disconnected")));
                    },
                }
            }

            let msg_link;
            match self.command_rx.try_recv() {
                Ok(ml) => {
                    msg_link = Some(ml);
                },
                Err(err) => match err {
                    mpsc::error::TryRecvError::Empty => {
                        msg_link = None;
                    },
                    mpsc::error::TryRecvError::Disconnected => {
                        return Err(
                            Box::new(
                                BeelayMqttClientError::new("Channel disconnected")));
                    },
                }
            }

            if let Some(msg_link) = msg_link {
                let resp;
                match msg_link.get_message() {
                    Command::Ping => {
                        trace!("Pong");
                        resp = CommandResponse::Ack;
                    },
                    Command::Set { switch_name, state } => {
                        debug!("Commanding {} to {} (simulated)", switch_name, state);
                        self.state_map.insert(switch_name.clone(), state.clone());
                        resp = CommandResponse::Ack;
                    },
                    Command::Stop => {
                        info!("Stopping MQTT simulation client");
                        should_run = false;
                        resp = CommandResponse::Ack;
                    },
                    Command::Reset => {
                        info!("Resetting MQTT simulation client (which doesn't do anything)");
                        resp = CommandResponse::Ack;
                    },
                }

                let resp_ch = msg_link.get_response_channel();
                resp_ch.send(resp).await?;
            }
            else if state_senders.len() > 0 {
                let elapsed = instant.elapsed();
                if elapsed > self.state_update_interval {
                    let state_senders = state_senders.clone();
                    if self.state_map.len() > 0 {
                    let state_update_sub_interval = self.state_update_interval / self.state_map.len() as u32; 
                        let state_map = self.state_map.clone();
                        tokio::spawn(async move {
                            let state_senders = state_senders;
                            for (name, state) in state_map {
                                for state_sender in &state_senders {
                                    if let Err(err) = state_sender.send((name.clone(), state.clone())).await {
                                        error!("Failed to send state update ({}, {}) because of {}", name, state, err);
                                    }
                                }

                                tokio::time::sleep(state_update_sub_interval).await;
                            }
                        });
                    }

                    instant = Instant::now();
                }
            }
            else {
                tokio::time::sleep(Duration::from_micros(500)).await;
            }
        }

        return Ok(())
    }
}

async fn process_notification(event: Event, base_topic: &str) -> Result<Option<(String, SwitchState)>, Box<dyn Error>> {
    match event {
        Event::Incoming(incoming) => {
            match incoming {
                rumqttc::Packet::Publish(publish) => {
                    let switch_name = &publish.topic[base_topic.len()+1..publish.topic.len()];
                    info!("Got packet for {}", switch_name);
                    let payload: serde_json::Value = serde_json::from_str(std::str::from_utf8(&publish.payload).unwrap())?;
                    let payload: serde_json::Map<String, serde_json::Value> = payload.as_object().unwrap().clone();
                    if let Some(state_value) = payload.get("state") {
                        let state = state_value.as_str().unwrap().to_string();
                        let state = state.to_lowercase();
                        debug!("Extracted state value: {}", state);
                        let state = str_to_switch_state(&state)?;
                        return Ok(Some((switch_name.to_string(), state)))
                    }
                }
                _ => {}
            }
        },
        _ => {}
    }
    Ok(None)
}

pub struct MqttClient {
    command_rx: mpsc::Receiver<MessageLink<Command, CommandResponse>>,
    state_link_rx: mpsc::Receiver<mpsc::Sender<(String, SwitchState)>>,
    switch_names: Vec<String>,
    host: String,
    port: u16, 
    base_topic: String
}

impl MqttClient {
    pub async fn run(&mut self) -> Result<(), Box<dyn Error>> {
        let mut state_senders = LinkedList::new();
        let mut should_run = true;
        while should_run {
            if let Err(err) = self.run_mqtt_instance(&mut state_senders, &mut should_run).await {
                error!("MQTT instance crashed: {}", err);
            }
        }

        Ok(())
    }

    async fn run_mqtt_instance(&mut self, state_senders: &mut LinkedList<mpsc::Sender<(String, SwitchState)>>, should_run_again: &mut bool) -> Result<(), Box<dyn Error>> {
        info!("Connecting to MQTT broker @ {}:{}", self.host, self.port);
        let mut mqttoptions = MqttOptions::new("rumqtt-async", self.host.clone(), self.port.clone());
        mqttoptions.set_keep_alive(Duration::from_secs(1));

        let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);

        for switch in &self.switch_names{
            client.subscribe(format!("{}/{}", self.base_topic, switch), QoS::AtMostOnce).await?;
        }

        info!("MQTT broker connected!");

        let (event_tx, mut event_rx) = mpsc::channel(256);

        tokio::spawn(async move {
            info!("Starting MQTT event loop");
            loop {
                match eventloop.poll().await {
                    Ok(event) => {
                        if let Err(err) = event_tx.send(event).await {
                            error!("Failed to pass along MQTT event: {}", err);
                        }
                    }
                    Err(err) => {
                        error!("MQTT event loop crashed: {}", err);
                        break;
                    }
                }
            }
        });

        let mut should_run = true;
        while should_run {
            match self.command_rx.try_recv() {
                Ok(msg_link) => {
                    let resp;
                    match msg_link.get_message() {
                        Command::Ping => {
                            trace!("Pong");
                            resp = CommandResponse::Ack;
                        },
                        Command::Set { switch_name, state } => {
                            let topic = format!("{}/{}/set", self.base_topic, switch_name);
                            let switch_state_str = switch_state_to_str(*state)?;
                            info!("Publishing: {} {}", topic, switch_state_str);
                            client.publish(topic, QoS::AtLeastOnce, false, switch_state_str).await?;
                            resp = CommandResponse::Ack;
                        },
                        Command::Stop => {
                            info!("Stopping MQTT client");
                            should_run = false;
                            *should_run_again = false;
                            resp = CommandResponse::Ack;
                        },
                        Command::Reset => {
                            should_run = false;
                            resp = CommandResponse::Ack;
                        },
                    }

                    let resp_ch = msg_link.get_response_channel();
                    resp_ch.send(resp).await?;
                },
                Err(err) => match err {
                    mpsc::error::TryRecvError::Empty => (),
                    mpsc::error::TryRecvError::Disconnected => {
                        return Err(
                            Box::new(
                                BeelayMqttClientError::new("Command channel disconnected")));
                    },
                }
            }

            self.process_next_state_link_request(state_senders).await?;
            self.process_next_event(&mut event_rx, state_senders).await?;

            tokio::time::sleep(Duration::from_micros(10)).await;
        }

        Ok(())
    }

    async fn process_next_state_link_request(&mut self, state_senders: &mut LinkedList<mpsc::Sender<(String, SwitchState)>>) -> Result<(), Box<dyn Error>> {
        match self.state_link_rx.try_recv() {
            Ok(tx) => {
                state_senders.push_back(tx);
            },
            Err(err) => match err {
                mpsc::error::TryRecvError::Empty => (),
                mpsc::error::TryRecvError::Disconnected => {
                    return Err(
                        Box::new(
                            BeelayMqttClientError::new("State link channel disconnected")));
                },
            }
        }

        Ok(())
    }

    async fn process_next_event(&mut self, event_rx: &mut mpsc::Receiver<Event>, state_senders: &mut LinkedList<mpsc::Sender<(String, SwitchState)>>) -> Result<(), Box<dyn Error>> {
        match event_rx.try_recv() {
            Ok(event) => {
                let event_cnt = match process_notification(event, &self.base_topic).await {
                    Ok(event_cnt) => event_cnt,
                    Err(err) => {
                        warn!("Failed to process MQTT event content: {}", err);
                        // Tolerate these failures since this can come from
                        // junk input from our connected friends.
                        return Ok(())
                    }
                };
                if let Some(event_cnt) = event_cnt {
                    if state_senders.len() > 0 {
                        for sender in state_senders {
                            sender.send(event_cnt.clone()).await?;
                        }
                    }
                    else {
                        error!("No state links! MQTT client yells into the abyss");
                    }
                }
            },
            Err(err) => match err {
                mpsc::error::TryRecvError::Empty => (),
                mpsc::error::TryRecvError::Disconnected => {
                    return Err(
                        Box::new(
                            BeelayMqttClientError::new("Event channel disconnected")));
                },
            }
        }

        Ok(())
    }
}

#[derive(Clone)]
pub struct MqttClientCtrl {
    msg_link_transactor: MessageLinkTransactor<Command, CommandResponse>,
    state_link_tx: mpsc::Sender<mpsc::Sender<(String, SwitchState)>>
}

impl MqttClientCtrl {
    pub async fn ping(&self) -> Result<Duration, Box<dyn Error>> {
        let instant = Instant::now();
        match self.msg_link_transactor.transact(Command::Ping).await {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(instant.elapsed())
                }
                else {
                    Err(MqttClientCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(MqttClientCtrl::none_response_err())
            }
        }
    }

    pub async fn set(&self, switch_name: &str, switch_state: SwitchState) -> Result<(), Box<dyn Error>> {
        let resp = self.msg_link_transactor.transact(Command::Set { switch_name: switch_name.to_string(), state: switch_state.clone() }).await;
        match resp {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(())
                }
                else {
                    Err(MqttClientCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(MqttClientCtrl::none_response_err())
            }
        }
    }

    pub async fn establish_state_update_link(&self, cap: usize) -> Result<mpsc::Receiver<(String, SwitchState)>, Box<dyn Error>> {
        let (tx, rx) = mpsc::channel(cap);
        self.state_link_tx.send(tx).await?;

        Ok(rx)
    }

    pub async fn reset(&self) -> Result<(), Box<dyn Error>> {
        match self.msg_link_transactor.transact(Command::Reset).await {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(())
                }
                else {
                    Err(MqttClientCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(MqttClientCtrl::none_response_err())
            }
        }
    }

    pub async fn stop(&self) -> Result<(), Box<dyn Error>> {
        match self.msg_link_transactor.transact(Command::Stop).await {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(())
                }
                else {
                    Err(MqttClientCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(MqttClientCtrl::none_response_err())
            }
        }
    }

    fn none_response_err() -> Box<BeelayMqttClientError> {
        Box::new(BeelayMqttClientError::new("None response from MqttClientCtrl channel"))
    }

    fn unexpected_response_err(resp: CommandResponse) -> Box<BeelayMqttClientError> {
        Box::new(BeelayMqttClientError::new(
            format!("Unexpected response from MqttClientCtrl channel: {}", resp).as_str()))
    }
}
