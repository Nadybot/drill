use hyper::{body, client::HttpConnector, Body, Client, Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};

pub struct DynamicAuthProvider {
    api_endpoint: Uri,
    client: Client<HttpsConnector<HttpConnector>>,
}

impl DynamicAuthProvider {
    pub fn new(api_endpoint: Uri) -> Self {
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();

        Self {
            api_endpoint,
            client: Client::builder().build(connector),
        }
    }

    pub async fn verify(&self, token: &str, desired_subdomain: &str) -> Option<String> {
        let uri = format!(
            "{}?token={}&desired_subdomain={}",
            self.api_endpoint, token, desired_subdomain
        );
        let req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();

        if let Ok(mut response) = self.client.request(req).await {
            let status = response.status();

            if status.is_success() {
                if let Ok(subdomain_bytes) = body::to_bytes(response.body_mut()).await {
                    if let Ok(subdomain) = String::from_utf8(subdomain_bytes.to_vec()) {
                        Some(subdomain)
                    } else {
                        log::error!("Token verification endpoint returned invalid UTF-8");
                        None
                    }
                } else {
                    log::error!(
                        "Failed to verify token at {}, rejecting client",
                        self.api_endpoint
                    );
                    None
                }
            } else {
                None
            }
        } else {
            log::error!(
                "Failed to verify token at {}, rejecting client",
                self.api_endpoint
            );
            None
        }
    }
}
