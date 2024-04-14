use std::{error::Error, fmt};

use const_format::concatcp;
use tokio::sync::mpsc;

pub const API_ELEM: &str = "api";
pub const CLIENT_ELEM: &str = "client";

pub const PATH_CSS_DIR: &str = ".css";
pub const PATH_CSS_MAIN_FILE: &str = "main.css";
pub const PATH_CSS_MAIN_PATH: &str = concatcp!(CLIENT_ELEM, "/", PATH_CSS_DIR, "/", PATH_CSS_MAIN_FILE);

pub const GENERIC_404_PAGE: &str = "
<!DOCTYPE html>
<html>
    <head>
        <title>Beelay 404 Not Found</title>
    </head>
    <body>
        <h1>Beelay 404 Not Found</h1>
        <p>Requested resource not found. Please consult Beelay documentation.</p>
    </body>
</html>
";

#[derive(Debug)]
pub struct BeelayCommonError {
    message: String
}

impl Error for BeelayCommonError {}

impl BeelayCommonError {
    fn new(message: &str) -> BeelayCommonError {
        BeelayCommonError{ message: message.to_string() }
    }
}

impl fmt::Display for BeelayCommonError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "BeelayCommonError: {}", self.message)
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum SwitchState {
    On,
    Off
}

impl fmt::Display for SwitchState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum SwitchStatus {
    Confirmed,
    Transitioning
}

impl fmt::Display for SwitchStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub fn switch_state_to_str(state: SwitchState) -> Result<String, Box<dyn Error>> {
    let state_str = match state {
        SwitchState::On => "on",
        SwitchState::Off => "off"
    };

    Ok(state_str.to_string())
}

pub fn str_to_switch_state(state_str: &str) -> Result<SwitchState, Box<dyn Error>> {
    let switch_state = match state_str {
        "on" => SwitchState::On,
        "off" => SwitchState::Off,
        _ => return Err(
            Box::new(
                BeelayCommonError::new(
                    format!("Can't resolve status string: {}", state_str).as_str())))
    };

    Ok(switch_state)
}

pub fn switch_status_to_str(status: SwitchStatus) -> Result<String, Box<dyn Error>> {
    let state_str = match status {
        SwitchStatus::Confirmed => "confirmed",
        SwitchStatus::Transitioning => "transitioning"
    };

    Ok(state_str.to_string())
}

pub fn str_to_switch_status(status_str: &str) -> Result<SwitchStatus, Box<dyn Error>> {
    let switch_state = match status_str {
        "confirmed" => SwitchStatus::Confirmed,
        "transitioning" => SwitchStatus::Transitioning,
        _ => return Err(
            Box::new(
                BeelayCommonError::new(
                    format!("Can't resolve status string: {}", status_str).as_str())))
    };

    Ok(switch_state)
}

#[derive(Clone)]
pub struct MessageLink<T1, T2> {
    message: T1,
    resp_channel: mpsc::Sender<T2>
}

impl<T1, T2> MessageLink<T1, T2> {
    pub fn get_message(&self) -> &T1 {
        &self.message
    }

    pub fn get_response_channel(&self) -> &mpsc::Sender<T2> {
        &self.resp_channel
    }
}

pub fn build_message_link<T1, T2>(message: T1, cap: usize) -> (MessageLink<T1, T2>, mpsc::Receiver<T2>) {
    let tx: mpsc::Sender<T2>;
    let rx: mpsc::Receiver<T2>;
    (tx, rx) = mpsc::channel(cap);

    let msg_link = MessageLink{ message: message, resp_channel: tx };
    
    (msg_link, rx)
}

#[derive(Clone)]
pub struct MessageLinkTransactor<T1, T2> {
    tx: mpsc::Sender<MessageLink<T1, T2>>
}

impl<T1, T2> MessageLinkTransactor<T1, T2> {
    pub async fn transact(&self, message: T1) -> Option<T2> {
        let (msg_link, mut rx) = build_message_link::<T1, T2>(message, 1);
        self.tx.send(msg_link).await
            .expect("Failed to send");

        rx.recv().await
    }
}

pub fn build_message_link_transactor<T1, T2>(cap: usize) -> (MessageLinkTransactor<T1, T2>, mpsc::Receiver<MessageLink<T1, T2>>) {
    let tx: mpsc::Sender<MessageLink<T1, T2>>;
    let rx: mpsc::Receiver<MessageLink<T1, T2>>;
    (tx, rx) = mpsc::channel(cap);

    let msg_link_trans = MessageLinkTransactor{ tx: tx };
    
    (msg_link_trans, rx)
}
