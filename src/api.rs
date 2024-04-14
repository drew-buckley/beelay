use std::{error::Error, fmt, collections::{VecDeque, HashMap}};
use hyper::{Response, Body, StatusCode, Method};
use serde_json;
use log::debug;

use crate::{common::{str_to_switch_state, switch_state_to_str, SwitchState}, core::{BeelayCoreCtrl, BeelayCoreError, BeelayCoreErrorType}};

const SWITCH_API_ELEM_NAME: &str = "switch";
const SWITCH_API_STATE_PARAM_NAME: &str = "state";
const SWITCH_API_DELAY_PARAM_NAME: &str = "delay";

#[derive(Debug)]
struct BeelyApiError {
    message: String
}

impl Error for BeelyApiError {}

impl BeelyApiError {
    fn new(message: &str) -> BeelyApiError {
        BeelyApiError{ message: message.to_string() }
    }
}

impl fmt::Display for BeelyApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "BeelyApiError: {}", self.message)
    }
}

fn generate_response(json: &str, status_code: StatusCode) -> Response<Body> {
    debug!("Generating response; JSON: {}; status code: {}", json, status_code);
    Response::builder()
        .status(status_code)
        .body(Body::from(json.to_string()))
        .expect("Failed to generate response")
}

fn generate_api_error_respose(error_message: &str, status_code: StatusCode) -> Response<Body> {
    generate_response(
        format!("{{\"error_message\":\"{}\"}}", error_message).as_str(), status_code)
}

fn generate_success_response(switch_state: Option<&str>) -> Response<Body> {
    let switch_state_str = match switch_state {
        Some(switch_state) => format!(",\"state\":\"{}\"", switch_state),
        None => "".to_string()
    };
    generate_response(
        format!("{{\"status\":\"success\"{}}}", switch_state_str).as_str(), StatusCode::OK)
}

fn generate_switch_list_response(switches: &Vec<String>) -> Response<Body> {
    let mut resp_map: HashMap<String, &Vec<String>> = HashMap::new();
    resp_map.insert("switches".to_string(), switches);
    let json = serde_json::to_string(&resp_map)
        .expect("Unable to serial switch list");

    generate_response(&json, StatusCode::OK)
}

pub struct BeelayApi {
    core_ctrl: BeelayCoreCtrl
}

impl BeelayApi {
    pub fn new(core_ctrl: BeelayCoreCtrl) -> BeelayApi {
        BeelayApi{ core_ctrl: core_ctrl }
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
            }
            _ => {
                let message = format!("Unknown endpoint element: {}", top_api_elem);
                return Ok(generate_api_error_respose(&message, StatusCode::NOT_FOUND))
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
            }
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
            return Ok(generate_switch_list_response(&switches))
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
        
        let (state, status) = match self.core_ctrl.get_switch_state(&switch_name).await {
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

        Ok(generate_success_response(Some(&state)))
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

        if let Err(err) = self.core_ctrl.set_switch_state(switch_name, new_state, delay).await {
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

        Ok(generate_success_response(None))
    }
}

// #[cfg(test)]
// mod tests {
//     use hyper::body;

//     use crate::core::RunMode;

//     use super::*;

//     async fn perform_test_routine(api: &BeelayApi, beelay: Arc<BeelayCore>) -> Result<(), Box<dyn Error>> {

//         for state in vec!["on", "off", "on", "off"] {
//             let path_vec = vec!["switch".to_string(), "switch1".to_string()];
//             api.handle_post(&path_vec, &vec![("state".to_string(), state.to_string())]).await
//                 .expect("Set switch failed");
//             let resp = api.handle_get(&path_vec, &Vec::new()).await
//                 .expect("Get switch failed");

//             let body = body::to_bytes(resp).await?;
//             let body = std::str::from_utf8(&body)?;
//             let content: HashMap<String, String> = serde_json::from_str(body)?;
//             let retrieved_state = content.get("state")
//                 .expect("Failed to retrieve state");
//             info!("State: {}", state.to_string());
//             assert!(state == retrieved_state);
//         }

//         beelay.stop().await?;

//         Ok(())
//     }

//     #[tokio::test]
//     async fn test_sim_end2end() {
//         let mut log_builder = env_logger::Builder::from_env(
//             env_logger::Env::default().default_filter_or("debug"));
//         log_builder.init();

//         let beelay = Arc::new(BeelayCore::new(&vec!["switch1".to_string(), "switch2".to_string()], "./test/run/", RunMode::Simulate));
//         let beelay_ref = Arc::clone(&beelay);
//         let api = BeelayApi::new(beelay);
//         tokio::join!(beelay_ref.run(), perform_test_routine(&api, Arc::clone(&beelay_ref)));
//     }
// }
