#![allow(unreachable_code)]
use anyhow::Result;
use oauth2::CsrfToken;
use rand::RngExt;
use rouille::router;
use serde::Deserialize;
use std::sync::mpsc::{self};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::config::Config;
use crate::send_error;

const AUTHORIZE: &str = "https://myanimelist.net/v1/oauth2/authorize";
const TOKEN: &str = "https://myanimelist.net/v1/oauth2/token";
// Injected at build time by build.rs (from the MAL_CLIENT_ID env var or a
// gitignored .env file) so it is not hard-coded in the source.
pub const CLIENT_ID: &str = env!("MAL_CLIENT_ID");


#[derive(Debug, Clone, Deserialize)]
pub struct Identity {
    pub token_type: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}



// this starts the local server that will wait for the reply from the OAuth provider, 
// and opens the browser to the login page.
pub fn oauth_login<F>(callback: F) -> (String, JoinHandle<()>)
where
    F: FnOnce(Identity) -> Result<()> + Send + Sync + 'static + Copy,
{
    let mut rng = rand::rng();
    let n: usize = rng.random_range(43..=128);

    let code_verifier =
        String::from_utf8(pkce::code_verifier(n)).expect("code verifier is valid ASCII");
    let state = CsrfToken::new_random().secret().to_string();

    if let Some((port, joinable)) =
        start_callback_server(callback, code_verifier.clone(), state.clone())
    {
        let url = create_oauth_url(port, &code_verifier, &state);
        (url, joinable)
    } else {
        panic!("Failed to start callback server");
    }
}

fn token_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

fn token_error(response: &mut ureq::http::Response<ureq::Body>) -> anyhow::Error {
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        anyhow::anyhow!("HTTP error: {}", status)
    } else {
        anyhow::anyhow!("HTTP error {}: {}", status, body)
    }
}

pub fn refresh_token<T: Into<String>, F>(refresh_token: T, callback: F) -> Result<()>
where
    F: FnOnce(Identity) -> Result<()> + Send + Sync,
{
    let body = [
        ("client_id", CLIENT_ID.to_string()),
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.into()),
    ];

    let mut response = token_agent().post(TOKEN).send_form(body).map_err(|e| {
        send_error!("refresh token request failed: {}", e);
        anyhow::anyhow!(e.to_string())
    })?;

    if !response.status().is_success() {
        let err = token_error(&mut response);
        send_error!("refresh token failed: {}", err);
        return Err(err);
    }

    let new_token = response.body_mut().read_json::<Identity>()?;

    callback(new_token)
}

fn create_oauth_url(port: u16, code_verifier: &str, state: &str) -> String {
    let redirect_uri = format!("http://localhost:{}/callback", port);

    // MAL only supports the "plain" PKCE method, so the challenge IS the
    // verifier verbatim (no SHA-256). The verifier/state characters are all
    // URL-safe, so no percent-encoding is needed.
    let url = format!(
        "{AUTHORIZE}?response_type=code\
            &client_id={CLIENT_ID}\
            &state={state}\
            &code_challenge={code_verifier}\
            &code_challenge_method=plain\
            &redirect_uri={redirect_uri}"
    );

    open::that(&url).expect("Failed to open browser");

    url
}

// a direct, blocking server-to-server POST to MAL's token endpoint.
// The response (access/refresh tokens) comes back in the body right away
fn exchange_for_user_tokens(code: &str, code_verifier: &str, port: u16) -> Result<Identity> {
    let redirect_uri = format!("http://localhost:{}/callback", port);

    let body = [
        ("client_id", CLIENT_ID),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", code_verifier),
    ];

    let mut response = token_agent().post(TOKEN).send_form(body).map_err(|e| {
        send_error!("token request failed: {}", e);
        anyhow::anyhow!(e.to_string())
    })?;

    if !response.status().is_success() {
        let err = token_error(&mut response);
        send_error!("token exchange failed: {}", err);
        return Err(err);
    }

    let identity = response.body_mut().read_json::<Identity>()?;

    Ok(identity)
}

/*
* This function starts a local server to listen for the callback from the OAuth provider.
* it takes a callback function as an argument, which will be called when the server receives a callback.
* The callback function should accept three parameters: access_token, refresh_token, and expires_in.
* It will return the port number on which the server is running.
* */
fn start_callback_server<F>(
    callback: F,
    code_verifier: String,
    expected_state: String,
) -> Option<(u16, thread::JoinHandle<()>)>
where
    F: FnOnce(Identity) -> Result<()> + Send + Sync + 'static + Copy,
{
    let port: u16 = Config::global().network.callback_port;
    let (tx, rx) = mpsc::channel::<()>();

    for i in 0..Config::global().network.max_port_retries {
        let _tx = tx.clone();
        // Clone per iteration so the move closure can capture fresh copies.
        let code_verifier = code_verifier.clone();
        let expected_state = expected_state.clone();
        let url = format!("0.0.0.0:{}", port + i);
        let result = rouille::Server::new(&url, move |request| {
            router!(request,
                (GET) (/callback) => {
                    let code = match request.get_param("code") {
                        Some(code) => code,
                        None => return rouille::Response::text("Missing code parameter").with_status_code(400)
                    };

                    let state = match request.get_param("state") {
                        Some(state) => state,
                        None => return rouille::Response::text("Missing state parameter").with_status_code(400)
                    };

                    // stop if missmatch
                    if state != expected_state {
                        return rouille::Response::text("State mismatch").with_status_code(400);
                    }

                    let results = match exchange_for_user_tokens(&code, &code_verifier, port + i){
                        Ok(exchange) => exchange,
                        Err(err) => {
                            return rouille::Response::text(format!("Failed to exchange code for tokens: {}", err)).with_status_code(500);
                        }
                    };

                    match callback(results) {
                        Ok(_) => {},
                        Err(err) => {
                            return rouille::Response::text(format!("Callback failed, error: {}", err));
                        },
                    }

                    // read the template file
                    let html_content = include_str!("../templates/success.html");

                    _tx.send(()).unwrap();
                    rouille::Response::html(html_content)
                },

                // the success page references this image instead of inlining it
                (GET) (/image) => {
                    rouille::Response::from_data(
                        "image/png",
                        include_bytes!("../templates/image.png").to_vec(),
                    )
                },

                _ => {
                    // println!("Got request for unknown path");
                    rouille::Response::empty_404()
                }
            )
        });

        match result {
            Ok(server) => {
                // println!("Server started on port {}", port);
                let (handle, sender) = server.stoppable();
                let joinable = thread::spawn(move || {
                    let _ = rx.recv();
                    // println!("Stopping server on {}", url);

                    thread::sleep(Duration::from_secs(1));
                    sender.send(()).unwrap();
                    handle.join().unwrap();
                    // println!("Server stopped");
                });

                return Some((port + i, joinable));
            }

            Err(_) => {
                // eprintln!("Failed to start server on {}: {}, retrying... port {}  ", url, err, port);
            }
        }
    }

    send_error!("Failed to start server after {} retries", Config::global().network.max_port_retries);
    None
}
