use std::{collections::{HashMap, HashSet, LinkedList}, error::Error, fmt, os::linux::raw::stat, sync::Arc, time::Duration};
use tokio::{self, time::Instant};
use tokio::sync::mpsc;
use rumqttc::{MqttOptions, AsyncClient, QoS, EventLoop, Event};
use log::{debug, error, info, log_enabled, warn};

use crate::common::{build_message_link_transactor, MessageLink, MessageLinkTransactor, SwitchState};

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
    Ack,
    Error{ error: String }
}

impl CommandResponse {
    fn is_ack(&self) -> bool {
        match &self {
            CommandResponse::Ack => true,
            _ => false
        }
    }
}

impl fmt::Display for CommandResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CommandResponse::Ack => {
                write!(f, "CommandResponse::Ack")
            },
            CommandResponse::Error{ error } => {
                write!(f, "CommandResponse::Error{{ error: {} }}", error)
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
                        debug!("Pong");
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
