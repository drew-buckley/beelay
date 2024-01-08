use std::collections::VecDeque;
use std::convert::Infallible;
use std::{sync::Arc, error::Error, net::SocketAddr};
use http::{StatusCode, Method};
use log::{debug, error, info, log_enabled, warn};

use hyper::{Body, Request, Response, Server, Uri};
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};

use crate::{core::BeelayCore, api::BeelayApi};

const GENERIC_404_PAGE: &str = "
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

const API_ELEM: &str = "api";
const CLIENT_ELEM: &str = "client";


fn parse_path_to_elems(path: &str) -> Vec<String> {
    let mut elems = Vec::new();
    for elem in path.split("/") {
        elems.push(elem.to_string());
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

async fn handle(api: Arc<BeelayApi>, req: Request<Body>) -> Result<Response<Body>, Infallible> {
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
        _ => {
            return Ok(generate_response(GENERIC_404_PAGE, StatusCode::NOT_FOUND))
        }
    }
}

pub struct BeelayService {
    core: Arc<BeelayCore>,
    api: Arc<BeelayApi>
}

impl BeelayService {
    pub fn new(core: BeelayCore, address: &str, port: &u16) -> BeelayService {
        let core = Arc::new(core);
        let api = Arc::new(BeelayApi::new(Arc::clone(&core)));

        let addr: SocketAddr = (address.to_string() + ":" + &port.to_string())
            .parse()
            .expect("Unable to parse socket address.");

        BeelayService{ 
            core: Arc::clone(&core),
            api: Arc::clone(&api)
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn Error>> {
        let core = Arc::clone(&self.core);
        tokio::task::spawn_local(async move {
            if let Err(err) = core.run().await {
                error!("Beelay core stopped running due to error: {}", err);
            }
        });
        
        self.perform_http_service().await
    }

    pub async fn stop(&self) -> Result<(), Box<dyn Error>> {
        self.core.stop().await
    }

    async fn perform_http_service(&self) -> Result<(), Box<dyn Error>> {
        let api = Arc::clone(&self.api);
        let make_service = make_service_fn(move |conn: &AddrStream| {
            let api = Arc::clone(&api);
            let addr = conn.remote_addr();
            let service = service_fn(move |req| {
                let api = Arc::clone(&api);
                handle(api, req)
            });
    
            async move { Ok::<_, Infallible>(service) }
        });

        Ok(())
    }
}
