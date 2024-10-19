use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::{sync::Arc, error::Error, net::SocketAddr};
use std::sync::atomic::Ordering::Relaxed;

use log::{debug, error, info};

use hyper::{Body, Request, Response, Server, StatusCode};
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use tokio::sync::{mpsc, Mutex};
use tokio::time::Instant;

use crate::common::{build_message_link_transactor, MessageLink, MessageLinkTransactor};
use crate::core::BeelayCoreCtrl;
use crate::{api::BeelayApi, frontend::BeelayFrontend, common::{GENERIC_404_PAGE, API_ELEM, CLIENT_ELEM}};

#[derive(Debug)]
pub struct BeelayServiceError {
    message: String
}

impl Error for BeelayServiceError {}

impl BeelayServiceError {
    fn new(message: &str) -> BeelayServiceError {
        BeelayServiceError{ message: message.to_string() }
    }
}

impl fmt::Display for BeelayServiceError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "BeelayServiceError: {}", self.message)
    }
}

#[derive(Clone)]
enum Command {
    Ping,
    Stop,
    Reset
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Command::Ping => {
                write!(f, "Command::Ping")
            },
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

fn parse_path_to_elems(path: &str) -> Vec<String> {
    let mut elems = Vec::new();
    for elem in path.split("/") {
        if !elem.is_empty() {
            elems.push(elem.to_string());
        }
    }

    elems
}

fn parse_query_params_to_pairs(query_params: Option<&str>) -> Vec<(String, String)> {
    let mut query_pairs = Vec::new();
    if let Some(query_params) = query_params {
        for query_pair in form_urlencoded::parse(query_params.as_bytes()) {
            let key = query_pair.0.as_ref().to_string();
            let value = query_pair.1.as_ref().to_string();
            query_pairs.push((key, value));
        }
    }

    query_pairs
}

fn generate_response(body: &str, status_code: StatusCode) -> Response<Body> {
    debug!("Generating generic response status code: {}", status_code);
    Response::builder()
        .status(status_code)
        .body(Body::from(body.to_string()))
        .expect("Failed to generate response")
}

async fn perform_http_service(addr: &SocketAddr, 
                              req_sender: Arc<tokio::sync::Mutex<async_channel::Sender<Request<Body>>>>,
                              resp_receiver: Arc<tokio::sync::Mutex<async_channel::Receiver<Response<Body>>>>) -> Result<(), Box<dyn Error>> {
    let req_sender = Arc::clone(&req_sender);
    let make_service = make_service_fn(move |_conn: &AddrStream| {
        let req_sender = Arc::clone(&req_sender);
        let resp_receiver = Arc::clone(&resp_receiver);
        // let addr = conn.remote_addr();
        let service = service_fn(move |req| {
            let req_sender = Arc::clone(&req_sender);
            let resp_receiver = Arc::clone(&resp_receiver);
            handle(req_sender, resp_receiver, req)
        });

        async move { Ok::<_, Infallible>(service) }
    });

    let server = Server::bind(&addr).serve(make_service);

    server.await?;

    Ok(())
}

async fn handle(req_sender: Arc<tokio::sync::Mutex<async_channel::Sender<Request<Body>>>>, 
                resp_receiver: Arc<tokio::sync::Mutex<async_channel::Receiver<Response<Body>>>>, 
                req: Request<Body>) -> Result<Response<Body>, Infallible> {
    {
        let req_sender = req_sender.lock().await;
        req_sender.send(req).await
            .expect("Failed to send response on internal channel");
    }

    let resp;
    {
        let resp_receiver = resp_receiver.lock().await;
        resp = resp_receiver.recv().await
            .expect("Failed to receive response on internal channel");
    }

    Ok(resp)
}

async fn inner_handle(api: Arc<BeelayApi>,
                      frontend: Arc<BeelayFrontend>,
                      req_receiver: Arc<tokio::sync::Mutex<async_channel::Receiver<Request<Body>>>>, 
                      resp_sender: Arc<tokio::sync::Mutex<async_channel::Sender<Response<Body>>>>) -> Result<(), Box<dyn Error>> {
    let req;
    {
        let req_receiver = req_receiver.lock().await;
        req = req_receiver.recv().await
            .expect("Failed to receive request on internal channel");
    }

    let resp = process_req(req, &api, &frontend).await.unwrap();
    {
        let resp_sender = resp_sender.lock().await;
        resp_sender.send(resp).await.expect("Failed to send response on internal channel");
    }

    Ok(())
}

async fn process_req(req: Request<Body>, api: &Arc<BeelayApi>, frontend: &Arc<BeelayFrontend>) -> Result<Response<Body>, Infallible> {
    let method = req.method();
    let uri = req.uri();
    let mut path = VecDeque::from_iter(parse_path_to_elems(uri.path()));
    let query_params = parse_query_params_to_pairs(uri.query());

    let top_path_elem = path.pop_front();
    if top_path_elem.is_none() {
        return Ok(generate_response(GENERIC_404_PAGE, StatusCode::NOT_FOUND))
    }
    let top_path_elem = top_path_elem.unwrap();
    match top_path_elem.as_str() {
        API_ELEM => {
            let api_path = Vec::from_iter(path);
            let result = api.handle_hit(method, &api_path, &query_params).await;
            match result {
                Ok(resp) => {
                    return Ok(resp)
                },
                Err(err) => {
                    error!("Error processing API request: {}", err);
                    return Ok(generate_response("{{\"status\":\"error\",\"error_message\":\"Internal error\"}}", StatusCode::INTERNAL_SERVER_ERROR))
                }
            }
        },
        CLIENT_ELEM => {
            let client_path = Vec::from_iter(path);
            let result = frontend.handle_hit(&client_path, &query_params).await;
            match result {
                Ok(resp) => {
                    return Ok(resp)
                },
                Err(err) => {
                    error!("Error processing client request: {}", err);
                    return Ok(generate_response("{{\"status\":\"error\",\"error_message\":\"Internal error\"}}", StatusCode::INTERNAL_SERVER_ERROR))
                }
            };
        },
        _ => {
            return Ok(generate_response(GENERIC_404_PAGE, StatusCode::NOT_FOUND))
        }
    }
}

pub struct BeelayService {
    command_rx: mpsc::Receiver<MessageLink<Command, CommandResponse>>,
    api: Arc<BeelayApi>,
    frontend: Arc<BeelayFrontend>,
    addr: SocketAddr,
    req_sender: Arc<tokio::sync::Mutex<async_channel::Sender<Request<Body>>>>,
    req_receiver: Arc<tokio::sync::Mutex<async_channel::Receiver<Request<Body>>>>,
    resp_sender: Arc<tokio::sync::Mutex<async_channel::Sender<Response<Body>>>>,
    resp_receiver: Arc<tokio::sync::Mutex<async_channel::Receiver<Response<Body>>>>,
    should_run: Arc<AtomicBool>
}

pub fn build_service(
    core_ctrl: BeelayCoreCtrl,
    switches: Vec<String>,
    address: &str,
    port: &u16,
    msg_queue_cap: usize,
    pretty_names: HashMap<String, String>,
    filters: HashMap<String, Vec<String>>
) -> (BeelayService, BeelayServiceCtrl) {
    let (mlt, rx) = build_message_link_transactor(msg_queue_cap);

    let ctrl = BeelayServiceCtrl {
        msg_link_transactor: mlt
    };

    let api = Arc::new(BeelayApi::new(core_ctrl.clone(), pretty_names.clone(), &switches, &filters));
    let frontend = Arc::new(BeelayFrontend::new(switches, pretty_names, &filters));

    let req_sender : async_channel::Sender<Request<Body>>;
    let req_receiver : async_channel::Receiver<Request<Body>>;
    (req_sender, req_receiver) = async_channel::unbounded();

    let resp_sender : async_channel::Sender<Response<Body>>;
    let resp_receiver : async_channel::Receiver<Response<Body>>;
    (resp_sender, resp_receiver) = async_channel::unbounded();

    let addr: SocketAddr = (address.to_string() + ":" + &port.to_string())
        .parse()
        .expect("Unable to parse socket address.");

    let service = BeelayService{
        command_rx: rx,
        api: Arc::clone(&api),
        frontend:  Arc::clone(&frontend),
        addr,
        req_sender: Arc::new(Mutex::new(req_sender)),
        req_receiver: Arc::new(Mutex::new(req_receiver)),
        resp_sender: Arc::new(Mutex::new(resp_sender)),
        resp_receiver: Arc::new(Mutex::new(resp_receiver)),
        should_run: Arc::new(AtomicBool::new(true))
    };

    (service, ctrl)
}

impl BeelayService {
    pub async fn run(&mut self) -> Result<(), Box<dyn Error>> {
        let req_sender = Arc::clone(&self.req_sender);
        let resp_receiver = Arc::clone(&self.resp_receiver);
        let addr = self.addr.clone();

        tokio::spawn(async move {
            if let Err(err) = perform_http_service(&addr,
                                                                   req_sender,
                                                                   resp_receiver).await {
                error!("HTTP service subsystem crashed: {}", err);
            }
        });

        let req_receiver = Arc::clone(&self.req_receiver);
        let resp_sender = Arc::clone(&self.resp_sender);
        let api = Arc::clone(&self.api);
        let frontend = Arc::clone(&self.frontend);
        let should_run = Arc::clone(&self.should_run);
        tokio::spawn(async move {
            let req_receiver = req_receiver;
            let resp_sender = resp_sender;
            let api = api;
            let frontend = frontend;
            let should_run = should_run;
            while should_run.load(Relaxed) {
                if let Err(err) = inner_handle(Arc::clone(&api), 
                                               Arc::clone(&frontend), 
                                               Arc::clone(&req_receiver), 
                                               Arc::clone(&resp_sender)).await {
                    error!("HTTP service failure: {}", err);
                }
            }
        });

        while self.should_run.load(Relaxed) {
            match self.command_rx.recv().await {
                Some(msg_link) => {
                    let resp;
                    match msg_link.get_message() {
                        Command::Ping => {
                            resp = CommandResponse::Ack;
                        },
                        Command::Stop => {
                            info!("Stopping HTTP service");
                            self.should_run.store(false, Relaxed);
                            resp = CommandResponse::Ack;
                        },
                        Command::Reset => {
                            resp = CommandResponse::Ack;
                        },
                    }

                    let resp_ch = msg_link.get_response_channel();
                    resp_ch.send(resp).await?;
                },
                None => {
                    error!("Received None from command channel");
                },
            }
        }

        Ok(())
    }

}

#[derive(Clone)]
pub struct BeelayServiceCtrl {
    msg_link_transactor: MessageLinkTransactor<Command, CommandResponse>
}

impl BeelayServiceCtrl {
    pub async fn ping(&self) -> Result<Duration, Box<dyn Error>> {
        let instant = Instant::now();
        match self.msg_link_transactor.transact(Command::Ping).await {
            Some(resp) => {
                if resp.is_ack() {
                    Ok(instant.elapsed())
                }
                else {
                    Err(BeelayServiceCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayServiceCtrl::none_response_err())
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
                    Err(BeelayServiceCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayServiceCtrl::none_response_err())
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
                    Err(BeelayServiceCtrl::unexpected_response_err(resp))
                }
            }
            None => {
                Err(BeelayServiceCtrl::none_response_err())
            }
        }
    }

    fn none_response_err() -> Box<BeelayServiceError> {
        Box::new(BeelayServiceError::new("None response from BeelayServiceCtrl channel"))
    }

    fn unexpected_response_err(resp: CommandResponse) -> Box<BeelayServiceError> {
        Box::new(BeelayServiceError::new(
            format!("Unexpected response from BeelayServiceCtrl channel: {}", resp).as_str()))
    }
}
