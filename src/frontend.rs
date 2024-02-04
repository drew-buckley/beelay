use std::{error::Error, fmt, collections::{VecDeque, HashMap}, sync::Arc};
use futures::SinkExt;
use http::{StatusCode, Method};
use hyper::{Response, Body};
use log::{debug, error, info, log_enabled, warn};

use crate::common::{GENERIC_404_PAGE, PATH_CSS_DIR, PATH_CSS_MAIN_FILE, PATH_CSS_MAIN_PATH};

const CSS_MAIN: &str = include_str!("frontend-assets/main.css");
const HTML_GUI_TEMPLATE: &str = include_str!("frontend-assets/gui-template.html.in");
const HTML_BUTTON_ELEM_TEMPLATE: &str = include_str!("frontend-assets/button-element-template.html.in");

fn generate_not_found_response() -> Response<Body> {
    debug!("Generating frontend 404 response");
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from(GENERIC_404_PAGE.to_string()))
        .expect("Failed to generate response")
}

fn generate_frontend_response(html: &str) -> Response<Body> {
    debug!("Generating frontend 200 response");
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(html.to_string()))
        .expect("Failed to generate response")
}

fn process_template(template: &str, sub_map: &HashMap<String, String>) -> String {
    let mut output = template.to_string();
    for (tag, value) in sub_map {
        output = output.replace(format!("{{{{{{{}}}}}}}", tag).as_str(), value);
    }

    output
}

fn process_gui_template(status: &str, buttons: &str, css_path: &str) -> String {
    let mut sub_map = HashMap::new();
    sub_map.insert("status".to_string(), status.to_string());
    sub_map.insert("buttons".to_string(), buttons.to_string());
    sub_map.insert("css_path".to_string(), css_path.to_string());

    process_template(HTML_GUI_TEMPLATE, &sub_map)
}

fn process_button_template(id: &str, label: &str, post_on_url: &str, post_off_url: &str, get_url: &str) -> String {
    let mut sub_map = HashMap::new();
    sub_map.insert("id".to_string(), id.to_string());
    sub_map.insert("label".to_string(), label.to_string());
    sub_map.insert("post_on_url".to_string(), post_on_url.to_string());
    sub_map.insert("post_off_url".to_string(), post_off_url.to_string());
    sub_map.insert("get_url".to_string(), get_url.to_string());

    process_template(HTML_BUTTON_ELEM_TEMPLATE, &sub_map)
}

pub struct BeelayFrontend {
    frontend_body: String
}

impl BeelayFrontend {
    pub fn new(switch_names: &Vec<String>) -> BeelayFrontend {
        let mut button_elems = Vec::with_capacity(switch_names.len());
        for switch_name in switch_names {
            let id = switch_name.replace(' ', "_").to_lowercase();
            let post_on_url = format!("/api/switch/{}/?state=on", switch_name);
            let post_off_url = format!("/api/switch/{}/?state=off", switch_name);
            let get_url = format!("/api/switch/{}", switch_name);

            button_elems.push(
                process_button_template(&id, &switch_name, &post_on_url, &post_off_url, &get_url));
        }
        let button_elem_str = button_elems.join("\n");
        BeelayFrontend { 
            frontend_body: process_gui_template("", &button_elem_str, PATH_CSS_MAIN_PATH) 
        }
    }

    pub async fn handle_hit(&self, gui_sub_path: &Vec<String>, _query_params: &Vec<(String, String)>) -> Result<Response<Body>, Box<dyn Error>> {
        let mut gui_sub_path = VecDeque::from_iter(gui_sub_path);
        match gui_sub_path.pop_front() {
            Some(sub_elem) => {
                match sub_elem.as_str() {
                    PATH_CSS_DIR => {
                        return self.handle_css_req(&mut gui_sub_path)
                    },
                    _ => return Ok(generate_not_found_response())
                };
            },
            None => {
                return Ok(generate_frontend_response(&self.frontend_body));
            }
        }
    }

    fn handle_css_req(&self, gui_sub_path: &mut VecDeque<&String>) -> Result<Response<Body>, Box<dyn Error>> {
        match gui_sub_path.pop_front() {
            Some(sub_elem) => {
                match sub_elem.as_str() {
                    PATH_CSS_MAIN_FILE => {
                        return Ok(generate_frontend_response(CSS_MAIN))
                    },
                    _ => {
                        return Ok(generate_not_found_response())
                    }
                }
            },
            None => {
                return Ok(generate_not_found_response())
            }
        };
    }
}
