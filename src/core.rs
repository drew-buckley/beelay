use rumqttc::{MqttOptions, AsyncClient, QoS, EventLoop};
use tokio::fs::{self, File};
use std::path::Path;
use std::time::Duration;
use std::{error::Error, fmt};
use std::collections::HashMap;
use tokio;
use async_channel;
use log::{debug, error, info, log_enabled, warn};

#[derive(Debug)]
struct BeelyCoreError {
    message: String
}

impl Error for BeelyCoreError {}

impl BeelyCoreError {
    fn new(message: &str) -> BeelyCoreError {
        BeelyCoreError{ message: message.to_string() }
    }
}

impl fmt::Display for BeelyCoreError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Oh no, something bad went down")
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum SwitchState {
    On,
    Off,
    Unknown
}

impl fmt::Display for SwitchState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

enum Command {
    Get{ switch_name: String },
    Set{ switch_name: String, state: SwitchState, delay: u16 },
    Stop
}

pub enum RunMode {
    Simulate,
    MqttLink{ host: String, port: u16, base_topic: String }
}

struct CommandLink {
    command: Command,
    result_sender: Option<async_channel::Sender<Result<SwitchState, Box<dyn Error>>>>,
}

struct MqttExecutor {
    mqtt_client: AsyncClient,
    event_loop: EventLoop,
    base_topic: String
}

pub struct BeelayCore {
    run_mode: RunMode,
    switch_names: Vec<String>,
    cmd_sender: async_channel::Sender<CommandLink>,
    cmd_receiver: async_channel::Receiver<CommandLink>,
    should_run: bool,
    state_cache_locks: HashMap<String, tokio::sync::Mutex<String>>
}

fn switch_state_to_str(state: SwitchState) -> Result<String, Box<dyn Error>> {
    let state_str = match state {
        SwitchState::On => "on",
        SwitchState::Off => "off",
        SwitchState::Unknown => return Err(
            Box::new(
                BeelyCoreError::new("Can't stringify switch state of Unknown")))
    };

    Ok(state_str.to_string())
}

fn str_to_switch_state(state_str: &str) -> Result<SwitchState, Box<dyn Error>> {
    let switch_state = match state_str {
        "on" => SwitchState::On,
        "off" => SwitchState::Off,
        _ => SwitchState::Unknown
    };

    Ok(switch_state)
}


impl BeelayCore {
    pub fn new(switch_names: &Vec<String>, switch_cache_dir: &str, run_mode: RunMode) -> BeelayCore {
        let cmd_sender : async_channel::Sender<CommandLink>;
        let cmd_receiver : async_channel::Receiver<CommandLink>;
        (cmd_sender, cmd_receiver) = async_channel::unbounded();

        let mut state_cache_locks: HashMap<String, tokio::sync::Mutex<String>> = HashMap::new();
        for switch_name in switch_names {
            let switch_cache_dir = Path::new(switch_cache_dir);
            let cache_file = switch_cache_dir.join(switch_name);
            let cache_file = cache_file.to_str().unwrap();

            state_cache_locks.insert(switch_name.clone(), tokio::sync::Mutex::new(cache_file.to_string()));
        }

        BeelayCore {
            run_mode: run_mode,
            switch_names: switch_names.clone(),
            cmd_sender: cmd_sender,
            cmd_receiver: cmd_receiver,
            should_run: true,
            state_cache_locks: state_cache_locks
        }
    }

    pub async fn set_switch_state(&self, switch_name: &str, state: SwitchState, delay: u16) -> Result<SwitchState, Box<dyn Error>> {
        let state_name = state.to_string();
        let cmd = Command::Set{switch_name: switch_name.to_string(), state, delay};
        let (cmd_link, res_receiver) = BeelayCore::create_command_link(cmd, true)?;

        let res_receiver = res_receiver.unwrap();

        debug!("Sending SET {} {}", switch_name, state_name);
        self.cmd_sender.send(cmd_link).await?;

        debug!("Waiting for response for SET {} {}", switch_name, state_name);
        let result = res_receiver.recv().await?;

        debug!("Got response for SET {} {}: {}", switch_name, state_name, result.is_ok());

        result
    }

    pub async fn get_switch_state(&self, switch_name: &str) -> Result<SwitchState, Box<dyn Error>> {
        let cmd = Command::Get{switch_name: switch_name.to_string()};
        let (cmd_link, res_receiver) = BeelayCore::create_command_link(cmd, true)?;

        debug!("Sending GET {}", switch_name);
        self.cmd_sender.send(cmd_link).await?;

        let res_receiver = res_receiver.unwrap();

        debug!("Waiting for response for GET {}", switch_name);
        let result = res_receiver.recv().await?;

        debug!("Got response for GET {}: {}", switch_name, result.is_ok());

        result
    }

    pub async fn stop(&self) -> Result<(), Box<dyn Error>> {
        let cmd = Command::Stop;
        let (cmd_link, _) = BeelayCore::create_command_link(cmd, false)?;

        debug!("Sending STOP");
        self.cmd_sender.send(cmd_link).await?;

        Ok(())
    }

    pub async fn run(&self) -> Result<(), Box<dyn Error>> {
        loop {
            let mut mqtt_executor: Option<MqttExecutor>;
            match &self.run_mode {
                RunMode::MqttLink{host, port, base_topic} => {
                    let mut mqttoptions = MqttOptions::new("rumqtt-async", host, port.clone());
                    mqttoptions.set_keep_alive(Duration::from_secs(1));

                    let (mqtt_client, event_loop) = AsyncClient::new(mqttoptions, 10);
                    for switch in &self.switch_names{
                        mqtt_client.subscribe(format!("{}/{}", base_topic, switch), QoS::AtMostOnce).await?;
                    }
                    
                    mqtt_executor = Some(MqttExecutor{ mqtt_client, event_loop, base_topic: base_topic.clone() });
                },
                RunMode::Simulate => {
                    mqtt_executor = None;
                }
            }

            let should_keep_running = self.inner_run(&mut mqtt_executor).await?;
            if !should_keep_running {
                break; // graceful exit
            }
        }

        Ok(())
    }

    async fn inner_run(&self, mqtt_executor: &mut Option<MqttExecutor>) -> Result<bool, Box<dyn Error>> {
        while self.should_run {
            if !self.cmd_receiver.is_empty() {
                let cmd_link = self.cmd_receiver.recv().await?;
                let cmd = cmd_link.command;
                let res_sender: Option<async_channel::Sender<Result<SwitchState, Box<dyn Error>>>> = cmd_link.result_sender;

                match cmd {
                    Command::Get{switch_name} => {
                        let result = self.inner_get(&switch_name, mqtt_executor).await;
                        if let Err(err) = res_sender.unwrap().send(result).await {
                            error!("Result sender failed: {}", err);
                            // May need to resend?
                            return Ok(true) // keep running
                        }
                    },
                    Command::Set{switch_name, state, delay} => {
                        let result = self.inner_set(&switch_name, state, delay, mqtt_executor).await;
                        if let Err(err) = res_sender.unwrap().send(result).await {
                            error!("Result sender failed: {}", err);
                            // May need to resend?
                            return Ok(true) // keep running
                        }
                    },
                    Command::Stop => {
                        return Ok(false) // stop running
                    }
                }
            }
            else {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }

            // if let Some(mqtt_executor) = mqtt_executor {
            //     let notification = mqtt_executor.event_loop.poll().await?;
            //     debug!("Received = {:?}", notification);
            // }
        }

        Ok(true) // keep running
    }

    async fn inner_get(&self, switch_name: &str, mut _mqtt_executor: &Option<MqttExecutor>) -> Result<SwitchState, Box<dyn Error>> {
        let state = self.read_cache_file(switch_name).await?;
        Ok(state)
    }

    async fn inner_set(&self, switch_name: &str, state: SwitchState, delay: u16, mqtt_executor: &Option<MqttExecutor>) -> Result<SwitchState, Box<dyn Error>> {
        if delay > 0 {
            tokio::time::sleep(Duration::from_secs(delay.into())).await;
        }

        if let Err(err) = self.write_cache_file(switch_name, state).await {
            error!("Failed to cache state for {}: {}", switch_name, err);
        }

        if let Some(mqtt_executor) = mqtt_executor {
            let mqtt_client = &mqtt_executor.mqtt_client;
            let base_topic = &mqtt_executor.base_topic;
            let switch_state_str = switch_state_to_str(state)?;
            mqtt_client.publish(format!("{}/{}/set", base_topic, switch_name), QoS::AtLeastOnce, false, switch_state_str).await?;
        }

        Ok(state)
    }

    fn create_command_link(command: Command, should_create_comms: bool) -> Result<(CommandLink, Option<async_channel::Receiver<Result<SwitchState, Box<dyn Error>>>>), Box<dyn Error>> {
        let res_sender : Option<async_channel::Sender<Result<SwitchState, Box<dyn Error>>>>;
        let res_receiver : Option<async_channel::Receiver<Result<SwitchState, Box<dyn Error>>>>;

        if should_create_comms {
            let res_s : async_channel::Sender<Result<SwitchState, Box<dyn Error>>>;
            let res_r : async_channel::Receiver<Result<SwitchState, Box<dyn Error>>>;
            (res_s, res_r) = async_channel::unbounded();

            res_sender = Some(res_s);
            res_receiver = Some(res_r);
        }
        else {
            res_sender = None;
            res_receiver = None;
        }

        Ok((CommandLink{ command: command, result_sender: res_sender}, res_receiver))
    }

    async fn read_cache_file(&self, switch_name: &str) -> Result<SwitchState, Box<dyn Error>> {
        let switch_cache_lock = self.get_cache_file(switch_name)?;
        let switch_cache = switch_cache_lock.lock().await;
        let switch_cache_path = Path::new((&switch_cache.as_str()).clone());

        debug!("Reading {}", switch_cache);

        let mut switch_state = SwitchState::Unknown;
        
        if fs::metadata(switch_cache_path).await.is_ok() {
            let cache_contents = fs::read_to_string(Path::new(&switch_cache.as_str())).await?;
            let cache_contents = cache_contents.to_ascii_lowercase();
            match cache_contents.as_str() {
                "on" => switch_state = SwitchState::On,
                "off" => switch_state = SwitchState::Off,
                _ => return Err(
                    Box::new(
                        BeelyCoreError::new(format!("Invalid cache value for {}: {}", switch_name, cache_contents).as_str())))
            }
        }

        Ok(switch_state)
    }

    async fn write_cache_file(&self, switch_name: &str, state: SwitchState) -> Result<(), Box<dyn Error>> {
        let switch_cache_lock = self.get_cache_file(switch_name)?;
        let switch_cache = switch_cache_lock.lock().await;
        let switch_cache_path = Path::new((&switch_cache.as_str()).clone());

        debug!("Writing {}", switch_cache);

        let state_str = match state {
            SwitchState::On => "on",
            SwitchState::Off => "off",
            SwitchState::Unknown => return Err(
                Box::new(
                    BeelyCoreError::new("Can't cache switch state of Unknown")))
        };

        fs::write(switch_cache_path, state_str).await?;

        Ok(())
    }

    fn get_cache_file(&self, switch_name: &str) -> Result<&tokio::sync::Mutex<String>, Box<dyn Error>> {
        let switch_cache_lock = self.state_cache_locks.get(switch_name);
        if switch_cache_lock.is_none() {
            return Err(
                Box::new(
                    BeelyCoreError::new(format!("Could not resolve cache file for switch name, {}", switch_name).as_str())))
        }

        Ok(switch_cache_lock.unwrap())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    async fn perform_test_routine(beelay: &BeelayCore) -> Result<(), Box<dyn Error>> {

        for state in vec![SwitchState::On, SwitchState::Off, SwitchState::On, SwitchState::Off] {
            beelay.set_switch_state("switch1", state, 0).await
                .expect("Set switch failed");
            let retrieved_state = beelay.get_switch_state("switch1").await
                .expect("Get switch failed");
            info!("State: {}", state.to_string());
            assert!(state == retrieved_state);
        }

        beelay.stop().await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_sim_end2end() {
        let mut log_builder = env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("debug"));
        log_builder.init();

        let beelay = BeelayCore::new(&vec!["switch1".to_string(), "switch2".to_string()], "./test/run/", RunMode::Simulate);
        tokio::join!(beelay.run(), perform_test_routine(&beelay));
    }
}