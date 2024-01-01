use tokio::fs::{self, File};
use std::path::Path;
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
    Get,
    Set{ state: SwitchState, delay: u16 }
}

struct CommandLink {
    switch_name: String,
    command: Command,
    result_sender: async_channel::Sender<Result<SwitchState, Box<dyn Error>>>,
}

pub struct BeelayCore {
    simulate: bool,
    switch_names: Vec<String>,
    cmd_sender: async_channel::Sender<CommandLink>,
    cmd_receiver: async_channel::Receiver<CommandLink>,
    should_run: bool,
    state_cache_locks: HashMap<String, tokio::sync::Mutex<String>>
}

impl BeelayCore {
    pub fn new(switch_names: &Vec<String>, switch_cache_dir: &str, simulate: bool) -> BeelayCore {
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
            simulate: simulate,
            switch_names: switch_names.clone(),
            cmd_sender: cmd_sender,
            cmd_receiver: cmd_receiver,
            should_run: true,
            state_cache_locks: state_cache_locks
        }
    }

    pub async fn set_switch_state(&self, switch_name: &str, state: SwitchState, delay: u16) -> Result<SwitchState, Box<dyn Error>> {
        let state_name = state.to_string();
        let (cmd_link, res_receiver) = BeelayCore::create_command_link(switch_name, Command::Set{state, delay})?;

        debug!("Sending SET {} {}", switch_name, state_name);
        self.cmd_sender.send(cmd_link).await?;

        debug!("Waiting for response for SET {} {}", switch_name, state_name);
        let result = res_receiver.recv().await?;

        debug!("Got response for SET {} {}: {}", switch_name, state_name, result.is_ok());

        result
    }

    pub async fn get_switch_state(&self, switch_name: &str) -> Result<SwitchState, Box<dyn Error>> {
        let (cmd_link, res_receiver) = BeelayCore::create_command_link(switch_name, Command::Get)?;

        debug!("Sending GET {}", switch_name);
        self.cmd_sender.send(cmd_link).await?;

        debug!("Waiting for response for GET {}", switch_name);
        let result = res_receiver.recv().await?;

        debug!("Got response for GET {}: {}", switch_name, result.is_ok());

        result
    }

    pub async fn run(&self) -> Result<(), Box<dyn Error>> {
        while self.should_run {
            let cmd_link = self.cmd_receiver.recv().await?;
            let cmd = cmd_link.command;
            let res_sender = cmd_link.result_sender;
            let switch_name = cmd_link.switch_name;

            match cmd {
                Command::Get => {
                    let result = self.inner_get(&switch_name).await;
                    if let Err(err) = res_sender.send(result).await {
                        error!("Result sender failed: {}", err);
                    }
                },
                Command::Set{state, delay} => {
                    let result = self.inner_set(&switch_name, state, delay).await;
                    if let Err(err) = res_sender.send(result).await {
                        error!("Result sender failed: {}", err);
                    }
                }
            }
        }
        Ok(())
    }

    async fn inner_get(&self, switch_name: &str) -> Result<SwitchState, Box<dyn Error>> {
        let state = self.read_cache_file(switch_name).await?;
        Ok(state)
    }

    async fn inner_set(&self, switch_name: &str, state: SwitchState, delay: u16) -> Result<SwitchState, Box<dyn Error>> {
        if let Err(err) = self.write_cache_file(switch_name, state).await {
            error!("Failed to cache state for {}: {}", switch_name, err);
        }

        Ok(state)
    }

    fn create_command_link(switch_name: &str, command: Command) -> Result<(CommandLink, async_channel::Receiver<Result<SwitchState, Box<dyn Error>>>), Box<dyn Error>> {
        let res_sender : async_channel::Sender<Result<SwitchState, Box<dyn Error>>>;
        let res_receiver : async_channel::Receiver<Result<SwitchState, Box<dyn Error>>>;
        (res_sender, res_receiver) = async_channel::unbounded();

        Ok((CommandLink{switch_name: switch_name.to_string(), command: command, result_sender: res_sender}, res_receiver))
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
            beelay.set_switch_state("switch1", SwitchState::On, 0).await?;
            let retrieved_state = beelay.get_switch_state("switch1").await?;
            info!("State: {}", state.to_string());
            assert!(state == retrieved_state);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_sim_end2end() {
        let mut log_builder = env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("debug"));
        log_builder.init();

        let beelay = BeelayCore::new(&vec!["switch1".to_string(), "switch2".to_string()], "./test/run/", true);
        tokio::join!(beelay.run(), perform_test_routine(&beelay));
    }
}
