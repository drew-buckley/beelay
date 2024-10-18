use std::{collections::{HashMap, VecDeque}, error::Error};
use hyper::{Response, Body, StatusCode, Method};
use serde::Serialize;
use serde_json;
use log::debug;

use crate::{common::{str_to_switch_state, switch_state_to_str, SwitchState, SwitchStatus}, core::{BeelayCoreCtrl, BeelayCoreError, BeelayCoreErrorType}};

const SWITCH_API_ELEM_NAME: &str = "switch";
const SWITCH_API_STATE_PARAM_NAME: &str = "state";
const SWITCH_API_DELAY_PARAM_NAME: &str = "delay";

const SWITCHES_API_ELEM_NAME: &str = "switches";

fn generate_response(json: &str, status_code: StatusCode) -> Response<Body> {
    debug!("Generating response; JSON: {}; status code: {}", json, status_code);
    Response::builder()
        .status(status_code)
        .body(Body::from(json.to_string()))
        .expect("Failed to generate response")
}

fn generate_api_error_respose(error_message: &str, status_code: StatusCode) -> Response<Body> {
    generate_response(
        format!("{{\"status\":\"error\",\"error_message\":\"{}\"}}", error_message).as_str(), status_code)
}

fn generate_state_success_response(switch_state: Option<&str>, transitioning: Option<bool>) -> Response<Body> {
    let switch_state_str = match switch_state {
        Some(switch_state) => format!(",\"state\":\"{}\"", switch_state),
        None => "".to_string()
    };
    let transitioning_str = match transitioning {
        Some(switch_state) => format!(",\"transitioning\":\"{}\"", switch_state.to_string()),
        None => "".to_string()
    };
    generate_response(
        format!("{{\"status\":\"success\"{}{}}}", switch_state_str, transitioning_str).as_str(), StatusCode::OK)
}

fn generate_switch_list_success_response(switches: &Vec<String>, pretty_names: &HashMap<String, String>) -> Response<Body> {
    #[derive(Serialize)]
    struct Response<'a> {
        switches: &'a Vec<String>,
        pretty_names: &'a HashMap<String, String>
    }

    let json = serde_json::to_string(&Response {
        switches,
        pretty_names
    }).expect("Unable to serial switch list");

    generate_response(&json, StatusCode::OK)
}

fn resolve_spaces(s: &str) -> String {
    s.replace("%20", " ")
}

pub struct BeelayApi {
    core_ctrl: BeelayCoreCtrl,
    pretty_names: HashMap<String, String>
}

impl BeelayApi {
    pub fn new(core_ctrl: BeelayCoreCtrl, pretty_names: HashMap<String, String>) -> BeelayApi {
        BeelayApi{ core_ctrl, pretty_names }
    }

    pub async fn handle_hit(&self, method: &Method, api_path: &Vec<String>, query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        match *method {
            Method::POST => {
                self.handle_post(&api_path, &query_params).await
            },
            Method::GET => {
                self.handle_get(&api_path, &query_params).await
            },
            _ => {
                let message = format!("Method not supported: {}", method);
                return Ok(generate_api_error_respose(&message, StatusCode::BAD_REQUEST))
            }
        }
    }

    async fn handle_post(&self, api_path: &Vec<String>, query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        let mut api_path = VecDeque::from_iter(api_path);
        let top_api_elem = api_path.pop_front();
        if top_api_elem.is_none() {
            return Ok(generate_api_error_respose("Only top level API URL provided", StatusCode::NOT_FOUND))
        }

        let top_api_elem = top_api_elem.unwrap();
        let resp;
        match top_api_elem.as_str() {
            SWITCH_API_ELEM_NAME => {
                resp = self.switch_post(&mut api_path, query_params).await?;
            },
            SWITCHES_API_ELEM_NAME => {
                let message = format!("POST not allowed for /{}", SWITCHES_API_ELEM_NAME);
                resp = generate_api_error_respose(&message, StatusCode::METHOD_NOT_ALLOWED);
            },
            _ => {
                let message = format!("Unknown endpoint element: {}", top_api_elem);
                resp = generate_api_error_respose(&message, StatusCode::NOT_FOUND);
            }
        }

        Ok(resp)
    }

    async fn handle_get(&self, api_path: &Vec<String>, query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        let mut api_path = VecDeque::from_iter(api_path);
        let top_api_elem = api_path.pop_front();
        if top_api_elem.is_none() {
            return Ok(generate_api_error_respose("Only top level API URL provided", StatusCode::NOT_FOUND))
        }

        let top_api_elem = top_api_elem.unwrap();
        let resp;
        match top_api_elem.as_str() {
            SWITCH_API_ELEM_NAME => {
                resp = self.switch_get(&mut api_path, query_params).await?;
            },
            SWITCHES_API_ELEM_NAME => {
                resp = self.switches_get(&mut api_path, query_params).await?;
            },
            _ => {
                let message = format!("Unknown endpoint element: {}", top_api_elem);
                return Ok(generate_api_error_respose(&message, StatusCode::NOT_FOUND))
            }
        }

        Ok(resp)
    }

    pub async fn switch_get(&self, api_sub_path: &mut VecDeque<&String>, _query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        let switch_name = api_sub_path.pop_front();
        if switch_name.is_none() {
            let switches = self.core_ctrl.get_switches().await?;
            return Ok(generate_switch_list_success_response(&switches, &self.pretty_names))
        }
        let switch_name = switch_name.unwrap();

        match api_sub_path.pop_front() {
            Some(elem) => {
                // Add sub elements here if they need to be made
                let message = format!("Unknown endpoint element: {}", elem);
                return Ok(generate_api_error_respose(&message, StatusCode::NOT_FOUND))
            },
            None => {}
        }
        
        let (state, status) = match self.core_ctrl.get_switch_state(&resolve_spaces(&switch_name)).await {
            Ok(state) => state,
            Err(err) => {
                match err.downcast_ref::<BeelayCoreError>() {
                    Some(err) => {
                        if err.get_type() == BeelayCoreErrorType::InvalidSwitch {
                            let message = format!("Unknown switch name: {}", switch_name);
                            return Ok(generate_api_error_respose(&message, StatusCode::BAD_REQUEST))
                        }
                    },
                    None => {}
                }

                let message = format!("Internal error: {}", err);
                return Ok(generate_api_error_respose(&message, StatusCode::INTERNAL_SERVER_ERROR))
            }
        };

        let state = match state {
            Some(state) => {
                match switch_state_to_str(state) {
                    Ok(state) => state,
                    Err(err) => {
                        let message = format!("Internal error: {}", err);
                        return Ok(generate_api_error_respose(&message, StatusCode::INTERNAL_SERVER_ERROR))
                    }
                }
            },
            None => "unknown".to_string()
        };

        let transitioning = match status {
            SwitchStatus::Confirmed => false,
            SwitchStatus::Transitioning => true
        };

        Ok(generate_state_success_response(Some(&state), Some(transitioning)))
    }

    pub async fn switches_get(&self, api_sub_path: &mut VecDeque<&String>, _query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        if let Some(elem) = api_sub_path.pop_front() {
            let message = format!("Unknown endpoint element: {}", elem);
            return Ok(generate_api_error_respose(&message, StatusCode::NOT_FOUND))
        };

        let switches = match self.core_ctrl.get_switches().await {
            Ok(switches) => switches,
            Err(err) => {
                let message = format!("Internal error: {}", err);
                return Ok(generate_api_error_respose(&message, StatusCode::INTERNAL_SERVER_ERROR))
            }
        };

        Ok(generate_switch_list_success_response(&switches, &self.pretty_names))
    }

    async fn switch_post(&self, api_sub_path: &mut VecDeque<&String>, query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        let switch_name = api_sub_path.pop_front();
        if switch_name.is_none() {
            return Ok(generate_api_error_respose("Only top level API URL provided", StatusCode::NOT_FOUND))
        }
        let switch_name = switch_name.unwrap();

        match api_sub_path.pop_front() {
            Some(elem) => {
                // Add sub elements here if they need to be made
                let message = format!("Unknown endpoint element: {}", elem);
                return Ok(generate_api_error_respose(&message, StatusCode::NOT_FOUND))
            },
            None => {}
        }

        let mut new_state: Option<SwitchState> = None;
        let mut delay: Option<u16> = None;
        for query_param in query_params {
            let key = &query_param.0;
            let value = &query_param.1;
            
            match key.as_str() {
                SWITCH_API_STATE_PARAM_NAME => {
                    let state = match str_to_switch_state(value) {
                        Ok(state) => state,
                        Err(_) => {
                            let message = format!("Invalid value for state parameter (must be 'on' or 'off'): {}", value);
                            return Ok(generate_api_error_respose(&message, StatusCode::BAD_REQUEST))
                        }
                    };
                    new_state = Some(state);
                },
                SWITCH_API_DELAY_PARAM_NAME => {
                    delay = match value.parse::<u16>() {
                        Ok(delay_value) => Some(delay_value),
                        Err(_) => {
                            let message = format!("Invalid value for delay parameter (must be integer): {}", value);
                            return Ok(generate_api_error_respose(&message, StatusCode::BAD_REQUEST))
                        }
                    };
                }
                _ => {
                    let message = format!("Unknown query parameter: {}", key);
                    return Ok(generate_api_error_respose(&message, StatusCode::NOT_FOUND))
                }
            }
        }

        if new_state.is_none() {
            return Ok(generate_api_error_respose("Endpoint requires 'state' query parameter", StatusCode::BAD_REQUEST))
        }
        let new_state = new_state.unwrap();

        if delay.is_none() {
            delay = Some(0);
        }
        let delay = delay.unwrap();

        if let Err(err) = self.core_ctrl.set_switch_state(&resolve_spaces(switch_name), new_state, delay).await {
            match err.downcast_ref::<BeelayCoreError>() {
                Some(err) => {
                    if err.get_type() == BeelayCoreErrorType::InvalidSwitch {
                        let message = format!("Unknown switch name: {}", switch_name);
                        return Ok(generate_api_error_respose(&message, StatusCode::BAD_REQUEST))
                    }
                },
                None => {}
            }

            let message = format!("Internal error: {}", err);
            return Ok(generate_api_error_respose(&message, StatusCode::INTERNAL_SERVER_ERROR))
        }

        let new_state = match switch_state_to_str(new_state) {
            Ok(state) => state,
            Err(err) => {
                let message = format!("Internal error: {}", err);
                return Ok(generate_api_error_respose(&message, StatusCode::INTERNAL_SERVER_ERROR))
            }
        };

        Ok(generate_state_success_response(Some(&new_state), Some(true)))
    }
}
