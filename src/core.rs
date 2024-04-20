use tokio::fs;
use tokio::time::Instant;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::{error::Error, fmt};
use std::collections::HashMap;
use tokio;
use tokio::sync::mpsc;
use log::{debug, error, info, warn};

use crate::common::{build_message_link_transactor, str_to_switch_state, str_to_switch_status, switch_state_to_str, switch_status_to_str, MessageLink, MessageLinkTransactor, SwitchState, SwitchStatus};
use crate::mqtt_client::MqttClientCtrl;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BeelayCoreErrorType {
    InvalidSwitch,
    InternalError
}

#[derive(Debug)]
pub struct BeelayCoreError {
    message: String,
    error_type: BeelayCoreErrorType
}

impl Error for BeelayCoreError {}

impl BeelayCoreError {
    fn new(message: &str, error_type: BeelayCoreErrorType) -> BeelayCoreError {
        BeelayCoreError{ message: message.to_string(), error_type }
    }

    pub fn get_type(&self) -> BeelayCoreErrorType {
        self.error_type
    }
}

impl fmt::Display for BeelayCoreError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "BeelayCoreError: {}", self.message)
    }
}

#[derive(Clone)]
enum Command {
    Ping,
    Get{ switch_name: String },
    Set{ switch_name: String, state: SwitchState, delay: u16 },
    EnumerateSwitches,
    Stop,
    Reset
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Command::Ping => {
                write!(f, "Command::Ping")
            }
            Command::Get{ switch_name } => {
                write!(f, "Command::Get{{ switch_name: {} }}", switch_name)
            },
            Command::Set{ switch_name, state, delay } => {
                write!(f, "Command::Set{{ switch_name: {}, state: {}, delay: {} }}", switch_name, state, delay)
            },
            Command::EnumerateSwitches => {
                write!(f, "Command::EnumerateSwitches")
            }
            Command::Stop => {
                write!(f, "Command::Stop")
            },
            Command::Reset => {
                write!(f, "Command::Reset")
            }
        }
    }
}

#[derive(Clone)]
enum CommandResponse {
    Ack,
    SwitchState{ name: String, state: Option<SwitchState>, status: SwitchStatus },
    SwitchEnumeration{ switches: Vec<String> },
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
            }
            CommandResponse::SwitchState{ name: switch_name, state, status} => {
                let state = match state {
                    Some(state) => format!("{}", state),
                    None => "None".to_string()
                };

                write!(f, "CommandResponse::SwitchState{{ switch_name: {}, state: {}, status: {} }}", switch_name, state, status)
            },
            CommandResponse::SwitchEnumeration{ switches } => {
                let switches_str = format!("[{}]", switches.join(", "));
                write!(f, "CommandResponse::SwitchEnumeration{{ switches: {} }}", switches_str)
            }
            CommandResponse::Error{ error } => {
                write!(f, "CommandResponse::Error{{ error: {} }}", error)
            }
        }
    }
}

async fn write_cache_file(state: SwitchState, status: SwitchStatus, switch_cache: &str) -> Result<(), Box<dyn Error>> {
    let switch_cache_path = Path::new(switch_cache);

    debug!("Writing {}", switch_cache);

    let state_str = switch_state_to_str(state)?;
    let status_str = switch_status_to_str(status)?;
    let cache_str = format!("{},{}", state_str, status_str);

    fs::write(switch_cache_path, &cache_str).await?;

    debug!("Successfully wrote {} to {}", cache_str, switch_cache);

    Ok(())
}

async fn read_cache_file(switch_cache: &str) -> Result<(Option<SwitchState>, SwitchStatus), Box<dyn Error>> {
    let switch_cache_path = Path::new(switch_cache);

    debug!("Reading {}", switch_cache);

    let mut switch_state = None;
    let mut switch_status = SwitchStatus::Confirmed;
    
    if fs::metadata(switch_cache_path).await.is_ok() {
        let cache_contents = fs::read_to_string(switch_cache_path).await?;
        debug!("Successfully read {} from {}", cache_contents, switch_cache);
        
        let cache_contents = cache_contents.to_ascii_lowercase();
        let cache_contents_vec: Vec<&str> = cache_contents.split(',').collect();

        switch_state = Some(str_to_switch_state(cache_contents_vec[0])?);
        switch_status = str_to_switch_status(cache_contents_vec[1])?;
    }

    Ok((switch_state, switch_status))
}

pub fn build_core(switch_names: &Vec<String>, switch_cache_dir: &str, mqtt_ctrl: MqttClientCtrl, msg_queue_cap: usize) -> (BeelayCore, BeelayCoreCtrl) {
    let mut state_cache_locks: HashMap<String, Arc<tokio::sync::Mutex<String>>> = HashMap::new();
    for switch_name in switch_names {
        let switch_cache_dir = Path::new(switch_cache_dir);
        let cache_file = switch_cache_dir.join(switch_name);
        let cache_file = cache_file.to_str().unwrap();

        state_cache_locks.insert(switch_name.clone(), Arc::new(tokio::sync::Mutex::new(cache_file.to_string())));
    }

    let (mlt, rx) = build_message_link_transactor(msg_queue_cap);
    let ctrl = BeelayCoreCtrl{ msg_link_transactor: mlt };

    let core = BeelayCore{ 
        switch_names: switch_names.clone(),
        state_cache_locks: Arc::new(state_cache_locks),
        command_rx: rx, 
        mqtt_ctrl
    };

    (core, ctrl)
}

pub struct BeelayCore {
    switch_names: Vec<String>,
    state_cache_locks: Arc<HashMap<String, Arc<tokio::sync::Mutex<String>>>>,
    command_rx: mpsc::Receiver<MessageLink<Command, CommandResponse>>,
    mqtt_ctrl: MqttClientCtrl
}

impl BeelayCore {
    pub async fn run(&mut self) -> Result<(), Box<dyn Error>> {
        let mut state_rx = self.mqtt_ctrl.establish_state_update_link(128).await?;
        let state_cache_locks = Arc::clone(&self.state_cache_locks);
        tokio::spawn(async move {
            let state_cache_locks = state_cache_locks;
            loop {
                match state_rx.recv().await {
                    Some((switch, state)) => {
                        debug!("Got state update of {},{}", switch, state);
                        if let Some(switch_cache_lock) = state_cache_locks.get(&switch) {
                            let switch_cache = switch_cache_lock.lock().await;
                            if let Err(err) = write_cache_file(state, SwitchStatus::Confirmed, &switch_cache).await {
                                error!("Failed to write {} because of {}", switch_cache, err);
                            }
                        }
                        else {
                            error!("Unrecognized switch name in state update: {}", switch);
                        }
                    },
                    None => {
                        warn!("State update channel down; core state no longer synching with MQTT");
                        return
                    },
                }
            }
        });

        let mut should_run = true;
        while should_run {
            let msg_link = self.command_rx.recv().await.unwrap();
            let resp;
            match msg_link.get_message() {
                Command::Ping => {
                    debug!("Pong");
                    resp = CommandResponse::Ack
                },
                Command::Get { switch_name } => {
                    match self.read_cache_file(switch_name).await {
                        Ok((state, status)) => {
                            resp = CommandResponse::SwitchState { name: switch_name.to_string(), state, status };
                        },
                        Err(err) => {
                            resp = CommandResponse::Error { error: err.to_string() }
                        }
                    };
                    
                }
                Command::Set { switch_name, state, delay } => {
                    info!("Setting {} to {} (delay {}s)", switch_name, state, delay);
                    let switch_name = switch_name.clone();
                    let switch_name_copy = switch_name.clone();
                    let state = state.clone();
                    let state_copy = state.clone();
                    let delay = *delay;
                    let mqtt_ctrl = self.mqtt_ctrl.clone();
                    let switch_cache_lock = self.get_cache_file(&switch_name)?;
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(delay as u64)).await;
                        if let Err(err) = mqtt_ctrl.set(switch_name.as_str(), state.clone()).await {
                            error!("Failed to send SET to MQTT client because of {}", err);
                            return
                        }
                        let switch_cache = switch_cache_lock.lock().await;
                        if let Err(err) = write_cache_file(state, SwitchStatus::Transitioning, &switch_cache).await {
                            error!("Failed to write {} because of {}", switch_cache, err);
                        }
                    });

                    resp = CommandResponse::SwitchState { name: switch_name_copy, state: Some(state_copy), status: SwitchStatus::Transitioning };
                },
                Command::Stop => {
                    info!("Stopping core");
                    should_run = false;
                    resp = CommandResponse::Ack;
                },
                Command::Reset => todo!(),
                Command::EnumerateSwitches => {
                    resp = CommandResponse::SwitchEnumeration { switches: self.switch_names.clone() };
                }
            }

            let resp_ch = msg_link.get_response_channel();
            resp_ch.send(resp).await?;
        }

        Ok(())
    }

    fn get_cache_file(&self, switch_name: &str) -> Result<Arc<tokio::sync::Mutex<String>>, Box<dyn Error>> {
        let switch_cache_lock = self.state_cache_locks.get(switch_name);
        if switch_cache_lock.is_none() {
            return Err(
                Box::new(
                    BeelayCoreError::new(
                        format!("Could not resolve cache file for switch name, {}", switch_name).as_str(), 
                        BeelayCoreErrorType::InternalError)))
        }

        Ok(Arc::clone(switch_cache_lock.unwrap()))
    }

    async fn read_cache_file(&self, switch_name: &str) -> Result<(Option<SwitchState>, SwitchStatus), Box<dyn Error>> {
        let switch_cache_lock = self.get_cache_file(switch_name)?;
        let switch_cache = switch_cache_lock.lock().await;
        let switch_state = read_cache_file(&switch_cache).await?;
        Ok(switch_state)
    }
}

#[derive(Clone)]
pub struct BeelayCoreCtrl {
    msg_link_transactor: MessageLinkTransactor<Command, CommandResponse>
}

impl BeelayCoreCtrl {
    pub async fn ping(&self) -> Result<Duration, Box<dyn Error>> {
        let instant = Instant::now();
        match self.msg_link_transactor.transact(Command::Ping).await {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(instant.elapsed())
                }
                else {
                    Err(BeelayCoreCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayCoreCtrl::none_response_err())
            }
        }
    }

    pub async fn set_switch_state(&self, switch_name: &str, switch_state: SwitchState, delay: u16) -> Result<(), Box<dyn Error>> {
        let resp = self.msg_link_transactor.transact(Command::Set { switch_name: switch_name.to_string(), state: switch_state.clone(), delay: delay }).await;
        match resp {
            Some(resp) => {
                if let CommandResponse::SwitchState{ name: _, state: _, status: _ } = resp {
                    //TODO?
                    Ok(())
                }
                else {
                    Err(BeelayCoreCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayCoreCtrl::none_response_err())
            }
        }
    }

    pub async fn get_switch_state(&self, switch_name: &str) -> Result<(Option<SwitchState>, SwitchStatus), Box<dyn Error>> {
        let resp = self.msg_link_transactor.transact(Command::Get { switch_name: switch_name.to_string() }).await;
        match resp {
            Some(resp) => {
                if let CommandResponse::SwitchState{ name, state, status } = resp {
                    assert!(name == switch_name);
                    Ok((state, status))
                }
                else {
                    Err(BeelayCoreCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayCoreCtrl::none_response_err())
            }
        }
    }

    pub async fn get_switches(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let resp = self.msg_link_transactor.transact(Command::EnumerateSwitches).await;
        match resp {
            Some(resp) => {
                if let CommandResponse::SwitchEnumeration{ switches } = resp {
                    Ok(switches)
                }
                else {
                    Err(BeelayCoreCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayCoreCtrl::none_response_err())
            }
        }
    }

    pub async fn reset(&self) -> Result<(), Box<dyn Error>> {
        match self.msg_link_transactor.transact(Command::Reset).await {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(())
                }
                else {
                    Err(BeelayCoreCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayCoreCtrl::none_response_err())
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
                    Err(BeelayCoreCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayCoreCtrl::none_response_err())
            }
        }
    }

    fn none_response_err() -> Box<BeelayCoreError> {
        Box::new(BeelayCoreError::new("None response from BeelayCoreCtrl channel", BeelayCoreErrorType::InternalError))
    }

    fn unexpected_response_err(resp: CommandResponse) -> Box<BeelayCoreError> {
        Box::new(BeelayCoreError::new(
            format!("Unexpected response from BeelayCoreCtrl channel: {}", resp).as_str(), 
                BeelayCoreErrorType::InternalError))
    }
}
