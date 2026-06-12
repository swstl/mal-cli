use crate::send_error;

use super::models::anime::{Anime, AnimeResponse, FavoriteAnime, FavoriteResponse, JikanData};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use super::models::user::User;
use cached::proc_macro::cached;
use database::DatabaseManager;
use std::fmt::Debug;
use std::io::Read;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use ureq::config::IpFamily;
use ureq::Agent;
use url::Url;

#[macro_export]
macro_rules! params {
    ($($key:expr => $value:expr),* $(,)?) => {
        vec![$(($key.to_string(), $value.to_string())),*]
    };
}

// this proxy url is just used to access a local cache server, for debugging and development
// pub const PROXY: &str = "http://localhost:1111/proxy?url=";
pub const PROXY: &str = "";
const MAX_RETRIES: u32 = 5;
const MAX_ERROR_BODY: usize = 2048;
static AGENT: OnceLock<Agent> = OnceLock::new();
fn get_agent() -> &'static Agent {
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .ip_family(IpFamily::Ipv4Only)
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn http_error_message(response: &mut ureq::http::Response<ureq::Body>) -> String {
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        format!("HTTP error: {}", status)
    } else {
        let body: String = body.chars().take(MAX_ERROR_BODY).collect();
        format!("HTTP error {}: {}", status, body)
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Identifier {
    pub auth_token: Option<String>,
    pub client_id: Option<String>,
}

impl Identifier {
    pub fn new(auth_token: Option<String>, client_id: Option<String>) -> Self {
        Self {
            auth_token,
            client_id,
        }
    }

    pub fn to_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let Some(token) = &self.auth_token {
            headers.push(("Authorization".to_string(), format!("Bearer {}", token)));
        }
        if let Some(client_id) = &self.client_id {
            headers.push(("X-MAL-Client-ID".to_string(), client_id.clone()));
        }
        headers
    }
}

#[cached(size = 2000, result = true)]
pub fn fetch_image(uri: String) -> Result<image::DynamicImage, String> {
    let url = Url::parse(&uri).map_err(|e| format!("Invalid URL: {}", e))?;

    let agent = get_agent();

    match url.scheme() {
        "http" | "https" => loop {
            match agent.get(&format!("{}{}", PROXY, uri)).call() {
                Ok(mut response) => {
                    if !response.status().is_success() {
                        return Err(http_error_message(&mut response));
                    }

                    let mut reader = response.body_mut().as_reader();
                    let mut buffer = Vec::new();
                    reader.read_to_end(&mut buffer).map_err(|e| e.to_string())?;

                    return image::load_from_memory(&buffer).map_err(|e| e.to_string());
                }
                Err(e) => {
                    let error_message = e.to_string().to_lowercase();
                    let error_is_timeout =
                        error_message.contains("timeout") || error_message.contains("timed out");

                    if !error_is_timeout {
                        return Err(format!("Request failed: {}", e));
                    }
                }
            }
        },
        "file" => {
            let path = url
                .to_file_path()
                .map_err(|_| "Invalid file URL".to_string())?;
            image::open(path).map_err(|e| e.to_string())
        }
        _ => Err("Unsupported URL scheme".to_string()),
    }
}

#[cached(size = 2000, result = true)]
pub fn fetch_anime(
    identifier: Identifier,
    url: String,
    parameters: Vec<(String, String)>,
) -> Result<AnimeResponse, Box<dyn std::error::Error>> {
    send_request::<AnimeResponse>(
        "GET", //
        url,
        parameters,
        identifier.to_headers(),
        None,
    )
}

// The single-anime endpoint (`/anime/{id}`) returns one anime object directly,
// not the `{ data: [...], paging }` list shape that `AnimeResponse` decodes, so
// it gets its own fetch that deserializes `Anime` on its own.
#[cached(size = 2000, result = true)]
pub fn fetch_single_anime(
    identifier: Identifier,
    url: String,
    parameters: Vec<(String, String)>,
) -> Result<Anime, Box<dyn std::error::Error>> {
    send_request::<Anime>(
        "GET", //
        url,
        parameters,
        identifier.to_headers(),
        None,
    )
}

#[cached(result = true)]
pub fn fetch_user(
    identifier: Identifier,
    url: String,
    parameters: Vec<(String, String)>,
) -> Result<User, Box<dyn std::error::Error>> {
    send_request::<User>(
        "GET", //
        url,
        parameters,
        identifier.to_headers(),
        None,
    )
}

// We scrape the favorites straight off the public MAL profile page
// (https://myanimelist.net/profile/<user>/favorites) instead of going through
// Jikan. Jikan proxies MAL from its own servers and intermittently fails to
// reach them (HTTP 200 with an `UpstreamException` body), whereas the user's
// own machine reaches MAL fine. The favorites are server-rendered HTML, so a
// small regex pass is enough to pull out id / title / image.
#[cached(result = true)]
pub fn fetch_favorited_anime(
    identifier: Identifier,
    url: String,
    parameters: Vec<(String, String)>,
) -> Result<FavoriteResponse, Box<dyn std::error::Error>> {
    let html = send_request_expect_text("GET", url, parameters, identifier.to_headers(), None)?;
    Ok(FavoriteResponse {
        data: JikanData {
            anime: parse_favorited_anime(&html),
        },
    })
}

// matches one favorited anime: the image link carries the id, the lazyloaded
// `data-src` image and the `alt` title all in one tag.
static FAVORITE_ANIME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"/anime/(\d+)/[^"]*"\s*>\s*<img[^>]*?data-src="([^"]+)"[^>]*?alt="([^"]*)""#)
        .expect("valid favorite-anime regex")
});

fn parse_favorited_anime(html: &str) -> Vec<FavoriteAnime> {
    // favorites are grouped into anime / manga / character / people containers;
    // restrict to the anime block so we don't pick up the others.
    let block = anime_favorites_block(html).unwrap_or(html);

    let mut seen = HashSet::new();
    FAVORITE_ANIME_RE
        .captures_iter(block)
        .filter_map(|caps| {
            let id = caps.get(1)?.as_str().parse::<usize>().ok()?;
            if !seen.insert(id) {
                return None;
            }
            Some(FavoriteAnime {
                id,
                image: full_size_image(caps.get(2)?.as_str()),
                title: decode_html_entities(caps.get(3)?.as_str()),
            })
        })
        .collect()
}

// slice out just the "boxlist-container anime" section, ending at whichever
// other favorites category comes next (if any).
fn anime_favorites_block(html: &str) -> Option<&str> {
    let start = html.find("boxlist-container anime")?;
    let rest = &html[start..];
    let end = ["boxlist-container manga", "boxlist-container character", "boxlist-container people"]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .min()
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

// MAL serves a resized thumbnail like ".../r/100x140/images/anime/1/2.jpg?s=..".
// Strip the "/r/<w>x<h>" segment and the cache-busting query for the full image.
fn full_size_image(url: &str) -> String {
    let url = url.split('?').next().unwrap_or(url);
    if let Some(idx) = url.find("/r/") {
        let after = &url[idx + "/r/".len()..];
        if let Some(slash) = after.find('/') {
            return format!("{}{}", &url[..idx], &after[slash..]);
        }
    }
    url.to_string()
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn build_url(
    base_url: &str,
    parameters: &[(String, String)],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut url = Url::parse(base_url)?;

    for (key, value) in parameters {
        url.query_pairs_mut().append_pair(key, value);
    }

    let target_url = url.to_string();
    Ok(format!("{}{}", PROXY, target_url))
}

// not cacheable since T
pub fn send_request<T>(
    method: &str,
    url: String,
    parameters: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<&str>,
) -> Result<T, Box<dyn std::error::Error>>
where
    T: serde::de::DeserializeOwned + Debug,
{
    let final_url =
        build_url(&url, &parameters).map_err(|e| format!("Failed to build proxied URL: {}", e))?;

    let agent = get_agent();

    for attempt in 0..MAX_RETRIES {
        // create request
        let result = match method {
            "GET" => {
                let mut request = agent.get(&final_url);
                for (key, value) in &headers {
                    request = request.header(key, value);
                }
                request.call()
            }

            "PATCH" => {
                let mut request = agent.patch(&final_url);
                for (key, value) in &headers {
                    request = request.header(key, value);
                }
                request.send(body.unwrap_or(""))
            }

            "PUT" => {
                let mut request = agent.put(&final_url);
                // .header("Content-type", "application/x-www-form-urlencoded")
                for (key, value) in &headers {
                    request = request.header(key, value);
                }
                request.send(body.unwrap_or(""))
            }

            "POST" => {
                let mut request = agent.post(&final_url);
                for (key, value) in &headers {
                    request = request.header(key, value);
                }
                request.send(body.unwrap_or(""))
            }

            "DELETE" => {
                let mut request = agent.delete(&final_url);
                for (key, value) in &headers {
                    request = request.header(key, value);
                }
                request.call()
            }

            _ => return Err(format!("Unsupported HTTP method: {}", method).into()),
        };

        match result {
            Ok(mut response) => {
                if !response.status().is_success() {
                    return Err(http_error_message(&mut response).into());
                }
                return Ok(response.body_mut().read_json::<T>()?);
            }

            // request failed due to network error or timeout etc
            Err(e) => {
                let error_message = e.to_string().to_lowercase();
                let error_is_timeout =
                    error_message.contains("timeout") || error_message.contains("timed out");

                if !error_is_timeout {
                    return Err(format!("Request failed: {}", e).into());
                }

                if attempt >= MAX_RETRIES - 1 {
                    return Err(format!("max retries exceeded: {}, {}", MAX_RETRIES, e).into());
                }

                let delay = Duration::from_millis(2000 * (attempt + 1) as u64);
                thread::sleep(delay);
            }
        }
    }

    Err("All retry attempts failed".into())
}

pub fn send_request_expect_text(
    method: &str,
    url: String,
    parameters: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let final_url =
        build_url(&url, &parameters).map_err(|e| format!("Failed to build proxied URL: {}", e))?;

    let agent = get_agent();

    for attempt in 0..MAX_RETRIES {
        let result = match method {
            "GET" => {
                let mut req = agent.get(&final_url);
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req.call()
            }
            "PATCH" => {
                let mut req = agent.patch(&final_url);
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req.send(body.unwrap_or(""))
            }
            "PUT" => {
                let mut req = agent.put(&final_url);
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req.send(body.unwrap_or(""))
            }
            "POST" => {
                let mut req = agent.post(&final_url);
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req.send(body.unwrap_or(""))
            }
            "DELETE" => {
                let mut req = agent.delete(&final_url);
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req.call()
            }
            _ => return Err(format!("Unsupported HTTP method: {}", method).into()),
        };

        match result {
            Ok(mut resp) => {
                if !resp.status().is_success() {
                    return Err(http_error_message(&mut resp).into());
                }
                return Ok(resp.body_mut().read_to_string()?);
            }
            Err(e) => {
                let em = e.to_string().to_lowercase();
                let is_timeout = em.contains("timeout") || em.contains("timed out");
                if !is_timeout {
                    return Err(format!("Request failed: {}", e).into());
                }
                if attempt >= MAX_RETRIES - 1 {
                    return Err(format!("max retries exceeded: {}, {}", MAX_RETRIES, e).into());
                }
                thread::sleep(Duration::from_millis(2000 * (attempt + 1) as u64));
            }
        }
    }

    Err("All retry attempts failed".into())
}

pub trait Fetchable: Sized {
    type Response;
    type Output;

    fn fetch(
        token: Identifier,
        url: String,
        parameters: Vec<(String, String)>,
    ) -> Result<Self::Response, Box<dyn std::error::Error>>;

    fn from_response(response: Self::Response) -> Self::Output;
}

pub trait Update: Sized + database::Entryable{
    type Response: serde::de::DeserializeOwned + Debug + Send;

    fn get_method(&self) -> &'static str;
    fn get_headers(&self, token: String) -> Vec<(String, String)>;
    fn get_parameters(&self) -> Vec<(String, String)>;
    fn get_body(&self) -> Option<String>;
    fn get_id(&self) -> usize;
    fn get_belonging_list(&self) -> String;
    fn to_offline_response(&self) -> Self::Response;
    fn pre_update(&mut self);

    fn update_local(
        mut self,
        database: &DatabaseManager,
    ) -> Result<(usize, Self::Response), Box<dyn std::error::Error>>
    {
        self.pre_update();
        let id = self.get_id();
        let response = self.to_offline_response();

        let result = if self.get_method() == "DELETE" {
            database.delete(&self)
        } else {
            database.upsert(self).map(|_| ())
        };

        if let Err(e) = result {
            send_error!("Failed to update local database: {}", e);
            return Err("local db error".into());
        }

        Ok((id, response))
    }

    fn update(
        &self,
        token: String,
        endpoint: String,
    ) -> Result<(usize, Self::Response), Box<dyn std::error::Error>> {
        let update = send_request::<Self::Response>(
            self.get_method(),
            endpoint,
            self.get_parameters(),
            self.get_headers(token),
            self.get_body().as_deref(),
        )?;
        Ok((self.get_id(), update))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_favorited_anime;

    // trimmed-down sample of a real MAL `/profile/<user>/favorites` page.
    const SAMPLE: &str = r#"
        <li><a href="https://myanimelist.net/anime/season">Seasonal Anime</a></li>
        <div class="boxlist-container anime mb16">
          <div class="boxlist col-4">
            <div class="di-tc">
              <a href="https://myanimelist.net/anime/42897/Horimiya">
                <img class="lazyload image profile-w48" src="https://cdn.myanimelist.net/images/spacer.gif" data-src="https://cdn.myanimelist.net/r/100x140/images/anime/1695/111486.jpg?s=abc" alt="Horimiya" />
              </a>
            </div>
            <div class="di-tc va-t pl8 data">
              <div class="title"><a href="https://myanimelist.net/anime/42897/Horimiya">Horimiya</a></div>
            </div>
          </div>
          <div class="boxlist col-4">
            <div class="di-tc">
              <a href="https://myanimelist.net/anime/9999/Title">
                <img class="lazyload image profile-w48" src="spacer.gif" data-src="https://cdn.myanimelist.net/images/anime/2/100.jpg" alt="Kaguya-sama &amp; &quot;Love&quot;" />
              </a>
            </div>
          </div>
        </div>
        <div class="boxlist-container manga mb16">
          <a href="https://myanimelist.net/anime/77777/InsideMangaBlock">
            <img data-src="https://cdn.myanimelist.net/images/anime/3/3.jpg" alt="ShouldBeExcluded" />
          </a>
        </div>
    "#;

    #[test]
    fn parses_anime_favorites_only() {
        let favs = parse_favorited_anime(SAMPLE);
        let ids: Vec<usize> = favs.iter().map(|f| f.id).collect();
        // the seasonal nav link and the entry inside the manga container are excluded
        assert_eq!(ids, vec![42897, 9999]);
    }

    #[test]
    fn strips_resize_segment_and_query_from_image() {
        let favs = parse_favorited_anime(SAMPLE);
        assert_eq!(
            favs[0].image,
            "https://cdn.myanimelist.net/images/anime/1695/111486.jpg"
        );
    }

    #[test]
    fn decodes_html_entities_in_title() {
        let favs = parse_favorited_anime(SAMPLE);
        assert_eq!(favs[1].title, "Kaguya-sama & \"Love\"");
    }
}
