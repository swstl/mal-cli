pub mod models;
pub mod network;
mod oauth;

use crate::config::Config;
use crate::mal::network::{Fetchable, Identifier};
use crate::{params, send_error};
use chrono::{Datelike, Local};
use database::{DatabaseManager, Entryable};
use models::anime::{Anime, AnimeId, FavoriteAnime, fields};
use models::user::User;
use network::Update;
pub use oauth::CLIENT_ID;
use oauth::{Identity, refresh_token};
use regex::Regex;
use std::any::type_name;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, thread::JoinHandle};

const BASE_URL: &str = "https://api.myanimelist.net/v2";
const CLIENT_FOLDER: &str = ".mal";
const CLIENT_FILE: &str = "client";
const SECONDS_IN_A_DAY: u64 = 86400;

type BoxedSendError = Box<dyn std::error::Error + Send + 'static>;

//TODO: encrypt the tokens
#[derive(Debug, Clone)]
pub struct MalClient {
    identity: Arc<RwLock<Option<Identity>>>,
    re: Regex,
    local_db: DatabaseManager, // uses this when user isnt signed in
}

impl MalClient {
    pub fn new(local_db: DatabaseManager) -> Self {
        let client = Self {
            identity: Arc::new(RwLock::new(None)),
            re: Regex::new(r"\(([0-9,]+)/([0-9,]+|Unknown)\)").unwrap(),
            local_db,
        };

        client.login_from_file();
        client
    }

    fn save_to_file(identity: &Identity) {
        let app_dir = Config::data_dir();
        let mal_dir = app_dir.join(CLIENT_FOLDER);
        if !mal_dir.exists() {
            fs::create_dir_all(&mal_dir)
                .map_err(|_| {
                    send_error!("Failed to create directory: {:?}", mal_dir);
                })
                .ok();
        }

        // refreshes token a week before it actually expires
        let expires_at = Self::time_now() + identity.expires_in;
        let data = format!(
            "mal_access_token = \"{}\"\nmal_refresh_token = \"{}\"\nmal_token_expires_at = \"{}\"",
            identity.access_token, identity.refresh_token, expires_at
        );

        let client_file = mal_dir.join("client");
        fs::write(client_file, data)
            .map_err(|_| {
                send_error!("Failed to write client file");
            })
            .ok();
    }

    pub fn time_now() -> u64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(n) => n.as_secs(),
            Err(_) => {
                send_error!("SystemTime failed");
                0
            }
        }
    }

    pub fn init_oauth() -> (String, JoinHandle<()>) {
        oauth::oauth_login(|identity| {
            Self::save_to_file(&identity);
            Ok(())
        })
    }

    pub fn login_from_file(&self) -> bool {
        let app_dir = Config::data_dir();
        if !app_dir.exists()
            || !app_dir
                .join(format!("{}/{}", CLIENT_FOLDER, CLIENT_FILE))
                .exists()
        {
            return false;
        }

        if let Ok(client_file) =
            fs::read_to_string(app_dir.join(format!("{}/{}", CLIENT_FOLDER, CLIENT_FILE)))
        {
            let mut at = String::new();
            let mut rt = String::new();
            let mut ea = 0;

            for line in client_file.lines() {
                match line {
                    l if l.starts_with("mal_access_token") => {
                        at = l.split("\"").nth(1).unwrap_or("").to_string()
                    }
                    l if l.starts_with("mal_refresh_token") => {
                        rt = l.split("\"").nth(1).unwrap_or("").to_string()
                    }
                    l if l.starts_with("mal_token_expires_at") => {
                        ea = l
                            .split("\"")
                            .nth(1)
                            .map(|s| s.parse::<u64>().unwrap_or(0))
                            .unwrap_or(0)
                    }
                    _ => {}
                }
            }

            // refresh token before it has expired
            if ea < Self::time_now() + (7 * SECONDS_IN_A_DAY) {
                match refresh_token(rt.clone(), |identity| {
                    Self::save_to_file(&identity);
                    at = identity.access_token;
                    rt = identity.refresh_token;
                    ea = identity.expires_in;
                    Ok(())
                }) {
                    Ok(_) => {}
                    Err(err) => {
                        send_error!("Token expired! please login again: {}", err);
                        return false;
                    }
                };
            }

            let mut identity = self.identity.write().unwrap();
            *identity = Some(Identity {
                token_type: "Bearer".to_string(),
                access_token: at,
                refresh_token: rt,
                expires_in: ea,
            });

            return true;
        }
        false
    }

    pub fn update_user_login(&self) {
        self.login_from_file();
    }

    pub fn log_out() {
        let app_dir = Config::data_dir();
        if !app_dir.exists()
            || !app_dir
                .join(format!("{}/{}", CLIENT_FOLDER, CLIENT_FILE))
                .exists()
        {
            return;
        }
        fs::remove_file(app_dir.join(format!("{}/{}", CLIENT_FOLDER, CLIENT_FILE)))
            .map_err(|_| {
                send_error!("Failed to remove client file");
            })
            .ok();
    }

    pub fn user_is_logged_in() -> bool {
        let app_dir = Config::data_dir();
        let client_file = app_dir.join(format!("{}/{}", CLIENT_FOLDER, CLIENT_FILE));

        if !client_file.exists() {
            return false;
        }

        if let Ok(content) = fs::read_to_string(&client_file) {
            return content.contains("mal_access_token");
        }

        false
    }

    pub fn get_client_id(&self) -> Option<String> {
        Some(CLIENT_ID.to_string())
    }

    pub fn current_season() -> (u16, String) {
        let now = Local::now();
        let year = now.year() as u16;
        let month = now.month();

        let season = match month {
            1..=3 => "winter",
            4..=6 => "spring",
            7..=9 => "summer",
            _ => "fall",
        };

        (year, season.to_string())
    }

    pub fn get_seasonal_anime(
        &self,
        year: u16,
        season: String,
        offset: usize,
        limit: usize,
    ) -> Option<Vec<Anime>> {
        self.send_request::<Anime>(
            format!(
                "{}/anime/season/{}/{}",
                BASE_URL,
                year,
                season.to_lowercase()
            ),
            params![
               "fields" => fields::ALL.join(","),
                "limit" => limit,
                "offset" => offset,
                "sort" => "anime_num_list_users",
                "nsfw" => &Config::global()
                    .allow_nsfw
                    .unwrap_or(false)
            ],
        )
    }

    pub fn get_suggested_anime(&self, offset: usize, limit: usize) -> Option<Vec<Anime>> {
        if !MalClient::user_is_logged_in() {
            return None;
        }
        self.send_request::<Anime>(
            format!("{}/anime/suggestions", BASE_URL),
            params![
                "fields" => fields::ALL.join(","),
                "limit" => limit,
                "offset" => offset,
                "nsfw" => &Config::global()
                    .allow_nsfw
                    .unwrap_or(false)
            ],
        )
    }

    pub fn get_top_anime(&self, filter: String, offset: usize, limit: usize) -> Option<Vec<Anime>> {
        self.send_request::<Anime>(
            format!("{}/anime/ranking", BASE_URL),
            params![
            "ranking_type" => filter,
            "fields" => fields::ALL.join(","),
            "limit" => limit,
            "offset" => offset,
            "nsfw" => &Config::global()
                .allow_nsfw
                .unwrap_or(false)

            ],
        )
    }

    pub fn search_anime(&self, query: String, offset: usize, limit: usize) -> Option<Vec<Anime>> {
        self.send_request::<Anime>(
            format!("{}/anime", BASE_URL),
            params![
                "q" => query,
                "fields" => fields::ALL.join(","),
                "limit" => limit,
                "offset" => offset,
                "nsfw" => &Config::global()
                    .allow_nsfw
                    .unwrap_or(false)
            ],
        )
    }

    // Fetch a single anime by id with the full field set. Unlike the list
    // endpoints this returns one anime object, so it goes through
    // `fetch_single_anime` rather than the `Fetchable`/list path.
    pub fn get_anime_by_id(&self, id: u64) -> Option<Anime> {
        let identity = self.identity.read().unwrap();
        let identifier = match identity.as_ref() {
            Some(token) => Identifier::new(Some(token.access_token.clone()), None),
            None => Identifier::new(None, self.get_client_id()),
        };

        // The timeline walks many anime sequentially, so keep each response
        // small: drop the bulkiest fields it never shows (recommendations embeds
        // whole anime objects; pictures is a gallery; statistics/background are
        // unused here). Everything the boxes and the popup read is kept.
        let heavy = [
            fields::STATISTICS,
            fields::RECOMMENDATIONS,
            fields::PICTURES,
            fields::BACKGROUND,
        ];
        let trimmed_fields = fields::ALL
            .iter()
            .filter(|f| !heavy.contains(f))
            .copied()
            .collect::<Vec<_>>()
            .join(",");

        match network::fetch_single_anime(
            identifier,
            format!("{}/anime/{}", BASE_URL, id),
            params!["fields" => trimmed_fields],
        ) {
            Ok(anime) => Some(anime),
            Err(e) => {
                send_error!("Error fetching anime {}: {:?}", id, e);
                None
            }
        }
    }

    pub fn get_user(&self) -> Option<User> {
        self.send_request::<User>(
            format!("{}/users/@me", BASE_URL),
            params![
                "fields" => "anime_statistics",
                "nsfw" => &Config::global()
                    .allow_nsfw
                    .unwrap_or(false)
            ],
        )
    }

    pub fn get_anime_list(
        &self,
        status: Option<String>,
        offset: usize,
        limit: usize,
    ) -> Option<Vec<Anime>> {
        if !MalClient::user_is_logged_in() {
            return None;
        }
        if !Self::user_is_logged_in() {
            return None;
        }
        self.get_anime_list_by_user("@me".to_string(), status, offset, limit)
    }

    pub fn get_anime_list_by_user(
        &self,
        username: String,
        status: Option<String>,
        offset: usize,
        limit: usize,
    ) -> Option<Vec<Anime>> {
        let mut parameters = params![
            "fields" => fields::ALL.join(","),
            "limit" => limit,
            "offset" => offset,
            "sort" => "list_updated_at",
            "nsfw" => &Config::global()
                .allow_nsfw
                .unwrap_or(false)
        ];

        if let Some(status) = status {
            parameters.push(("status".to_string(), status));
        }

        self.send_request::<Anime>(
            format!("{}/users/{}/animelist", BASE_URL, username),
            parameters,
        )
    }

    pub fn get_favorited_anime(&self, username: String) -> Option<Vec<FavoriteAnime>> {
        self.send_request::<FavoriteAnime>(
            format!("https://myanimelist.net/profile/{}/favorites", username),
            params![],
        )
    }

    pub fn update_user_list<T: Update + Entryable + Clone>(
        &self,
        element: T,
    ) -> Result<(usize, T::Response), Box<dyn std::error::Error + 'static>> {
        if !Self::user_is_logged_in() {
            return element.update_local(&self.local_db);
        };

        let token = self
            .identity
            .read()
            .unwrap()
            .as_ref()
            .map(|id| id.access_token.clone())
            .ok_or_else(|| send_error!("You need to log in to use any list functions"));

        let token = match token {
            Ok(t) => t,
            Err(_) => return Err("token error".into()),
        };

        let local_result = element
            .clone()
            .update_local(&self.local_db);

        match element.update(
            token,
            format!(
                "{}/{}/{}/my_list_status",
                BASE_URL,
                element.get_belonging_list(),
                element.get_id()
            ),
        ) {
            Ok(remote) => Ok(remote),
            Err(e) => {
                send_error!("Failed to sync update to MAL: {}", e);
                local_result
            }
        }
    }

    pub fn update_user_list_async<T: Update + Send + 'static + Entryable + Clone>(
        &self,
        element: T,
    ) -> tokio::task::JoinHandle<Result<(usize, T::Response), BoxedSendError>>
    where
        T::Response: Send,
    {
        let client = self.clone();
        tokio::task::spawn_blocking(move || {
            client.update_user_list(element).map_err(|e| -> BoxedSendError {
                Box::new(std::io::Error::other(e.to_string()))
            })
        })
    }

    // this a very specific request i must say (gets the number of available episodes for an anime)
    pub fn get_available_episodes(
        &self,
        anime_id: AnimeId,
    ) -> Result<Option<u32>, Box<dyn std::error::Error>> {
        let url = format!(
            "https://myanimelist.net/anime/{}/thiscanbewhatever/episode",
            anime_id
        );
        let mut response = ureq::get(&url).call()?;
        let html = response.body_mut().read_to_string()?;

        if let Some(captures) = self.re.captures(&html)
            && let Some(available_str) = captures.get(1)
        {
            let cleaned = available_str.as_str().replace(",", "");
            return Ok(Some(cleaned.parse::<u32>()?));
        }
        Ok(None)
    }

    fn send_request<T>(&self, url: String, parameters: Vec<(String, String)>) -> Option<T::Output>
    where
        T: Fetchable,
    {
        let identity = self //
            .identity
            .read()
            .unwrap();

        let identifier = match identity.as_ref() {
            Some(id) => Identifier::new(
                // user credentials
                Some(id.access_token.clone()),
                None,
            ),
            None => Identifier::new(
                // app credentials
                None,
                self.get_client_id(),
            ),
        };

        let response = T::fetch(identifier, url, parameters);
        let response = match response {
            Ok(response) => response,
            Err(e) => {
                send_error!("Error fetching {}: {:?}", type_name::<T>(), e);
                return None;
            }
        };
        Some(T::from_response(response))
    }
}
