mod video_player;
pub mod fzf;
pub mod mpv;

use crate::{mal::models::anime::Anime, player::video_player::VideoPlayer};
use crate::config::Config;
pub use self::video_player::PlayError;
pub use self::video_player::PlayResult;

use std::io::{BufRead, BufReader};
use std::{process::{Command, Stdio}};
use shell_escape::escape;

/// Strip ANSI/terminal control sequences from a string so it renders cleanly
/// in the TUI error box. ani-cli colours its output (e.g. `die` uses red),
/// and those raw escape bytes otherwise break ratatui's width/wrapping.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // CSI sequence: ESC '[' params... final-byte (0x40..=0x7e)
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('\x40'..='\x7e').contains(&p) {
                            break;
                        }
                    }
                }
            }
            '\r' => {} // carriage returns reset the cursor; drop them
            _ => out.push(c),
        }
    }
    out
}

pub struct AnimePlayer {
    video_player: VideoPlayer,
}

impl std::fmt::Display for PlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayError::NotReleased(anime) => write!(
                f,
                "\"{}\"\nis not yet released.",
                if anime.alternative_titles.en.is_empty() {
                    anime.title.clone()
                } else {
                    anime.alternative_titles.en.clone()
                }
            ),
            PlayError::CommandFailed {
                stderr,
                exit_code,
                stdout,
            } => {
                write!(
                    f,
                    "ani-cli replied:\nError: {}\nExit code: {}\nOutput: {}",
                    stderr, exit_code, stdout
                )
            }
            PlayError::NotFound(msg) => write!(f, "Can't seem to find ani-cli: \n{}", msg),
            PlayError::NoResults(msg) => write!(
                f,
                "ani-cli replied:\nError: {}\nthe anime might not be available yet",
                msg
            ),
            PlayError::Other(msg) => write!(f, "Error running ani-cli: \n{}", msg),
        }
    }
}

impl AnimePlayer {
    pub fn new() -> Self {
        AnimePlayer {
            video_player: VideoPlayer::new(),
        }
    }

    pub fn play_anime(&self, anime: &Anime, episode: u32) -> Result<PlayResult, PlayError> {
        for bin in ["ani-cli"] {
            if !is_in_path(bin) {
                return Err(PlayError::NotFound(format!("{} is not installed or not in PATH", bin)));
            }
        }
        // hook
        if let Some(hook) = Config::global().player.launching_hook.clone()
            && let Err(e) = self.run_command(&hook, anime, episode, None, None)
        {
            eprintln!("Failed to run launching hook: {}", e);
        };

        ratatui::restore();


        let loc = self.extract_url(anime, episode).map_err(|e| PlayError::Other(e.to_string()))?;

        // hook
        if let Some(hook) = Config::global().player.pre_playback_hook.clone()
            && let Err(e) = self.run_command(&hook, anime, episode, Some(&loc), None)
        {
            eprintln!("Failed to run pre-playback hook: {}", e);
        };

        let result = if Config::global().player.disable_default_player {
            PlayResult {
                current_time: "00:00:00".to_string(),
                total_time: "00:00:00".to_string(),
                is_completed: false,
                fully_watched: false,
                percentage: 0,
                episode,
            }
        } else {
            self.video_player.play(&loc, episode)?
        };

        // hook
        if let Some(hook) = Config::global().player.post_playback_hook.clone()
            && let Err(e) = self.run_command(&hook, anime, episode, Some(&loc), Some(&result))
        {
            eprintln!("Failed to run post-playback hook: {}", e);
        };

        // mark as completed
        if Config::global().player.always_complete_episode {
            return Ok(PlayResult {
                current_time: "00:00:00".to_string(),
                total_time: "00:00:00".to_string(),
                is_completed: true,
                fully_watched: true,
                percentage: 100,
                episode,
            });
        }

        Ok(result)
    }

    pub fn extract_url(&self, anime: &Anime, episode: u32) -> Result<(String, Option<String>), PlayError> {
        let exe = std::env::current_exe().map_err(|e| PlayError::Other(e.to_string()))?;
        let shim_dir = std::env::temp_dir().join(format!("mal-tui-{}", std::process::id()));
        std::fs::create_dir_all(&shim_dir).map_err(|e| PlayError::Other(e.to_string()))?;
        for name in ["fzf", "mpv"] {
            let link = shim_dir.join(name);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&exe, &link).map_err(|e| PlayError::Other(e.to_string()))?;
        }
        let new_path = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap_or_default());

        let mut dub = String::new();
        if Config::global().dub.unwrap_or(false) {
            dub.push_str("--dub");
        }

        let mut child = Command::new("ani-cli")
            .env("PATH", new_path)
            .env("ANICLI_TARGET", &anime.title)
            .arg(dub)
            .arg("--no-detach")
            .arg("--exit-after-play")
            .arg("-e")
            .arg(episode.to_string())
            .arg(&anime.title)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| PlayError::NotFound(e.to_string()))?;

        println!("spawned ani-cli with pid {}", child.id());

        // Drain stderr on its own thread: prints ani-cli's progress/error
        // output live and captures it so we can surface the real failure
        // reason (ani-cli reports failures via `die` on stderr). Draining
        // both pipes concurrently also avoids a deadlock if one fills up.
        let child_stderr = child.stderr.take()
            .ok_or_else(|| PlayError::Other("failed to capture ani-cli stderr".to_string()))?;
        let stderr_handle = std::thread::spawn(move || {
            let mut captured = String::new();
            for line in BufReader::new(child_stderr).lines().map_while(Result::ok) {
                eprintln!("{}", line);
                captured.push_str(&line);
                captured.push('\n');
            }
            captured
        });

        // Read stdout line-by-line so output appears live, while still
        // capturing every line so we can find the marker afterwards.
        let child_stdout = child.stdout.take()
            .ok_or_else(|| PlayError::Other("failed to capture ani-cli stdout".to_string()))?;
        let mut lines = Vec::new();
        for line in BufReader::new(child_stdout).lines() {
            let line = line.map_err(|e| PlayError::Other(e.to_string()))?;
            // Don't echo the internal marker line to the user.
            if !line.contains("__MAL_MPV__") {
                println!("{}", line);
            }
            lines.push(line);
        }

        let status = child.wait().map_err(|e| PlayError::Other(e.to_string()))?;
        let stderr = stderr_handle.join().unwrap_or_default();

        let marker = lines
            .iter()
            .find(|l| l.contains("__MAL_MPV__"))
            .ok_or_else(|| {
                // ani-cli reports the failure reason via `die` on its last
                // stderr line; earlier lines are just progress noise.
                let clean = strip_ansi(&stderr);
                let reason = clean
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("ani-cli gave no reason");
                PlayError::Other(format!(
                    "ani-cli exited with {} before returning a video:\n{}",
                    status,
                    reason.trim()
                ))
            })?;

        let mut parts = marker.split('\t');
        parts.next(); // skip the marker token
        let url = parts.next().unwrap_or("").to_string();
        let referrer = parts.next().filter(|s| !s.is_empty()).map(|s| s.to_string());

        if url.is_empty() {
            return Err(PlayError::Other("ani-cli did not return a URL".to_string()));
        }

        Ok((url, referrer))
    }

    fn run_command(
        &self,
        command: &str,
        anime: &Anime,
        episode: u32,
        url: Option<&(String, Option<String>)>,
        result: Option<&PlayResult>,
    ) -> Result<(), String> {
        let cmd = command
            .replace("{title}", &escape(anime.title.clone().into()))
            .replace("{episode}", &escape(episode.to_string().into()))
            .replace("{url}", &escape(url.map(|u| u.0.as_str()).unwrap_or("").into()))
            .replace("{referer}", &escape(url.and_then(|u| u.1.as_deref()).unwrap_or("").into()))
            .replace("{referrer}", &escape(url.and_then(|u| u.1.as_deref()).unwrap_or("").into()))
            .replace("{current_time}", &escape(result.map(|r| r.current_time.clone()).unwrap_or_default().into()))
            .replace("{total_time}", &escape(result.map(|r| r.total_time.clone()).unwrap_or_default().into()))
            .replace("{percentage}", &escape(result.map(|r| r.percentage.to_string()).unwrap_or_default().into()))
            .replace("{is_completed}", &escape(result.map(|r| r.is_completed.to_string()).unwrap_or_default().into()))
            .replace("{fully_watched}", &escape(result.map(|r| r.fully_watched.to_string()).unwrap_or_default().into()));

        #[cfg(unix)]
        let status = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .status()
            .map_err(|e| format!("Failed to run hook: {}", e))?;

        #[cfg(windows)]
        let status = Command::new("cmd")
            .arg("/C")
            .arg(&cmd)
            .status()
            .map_err(|e| format!("Failed to run hook: {}", e))?;

        if !status.success() {
            return Err(format!("Hook exited with status: {:?}", status.code()));
        }

        Ok(())
    }
}

fn is_in_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false; };
    path.split(':').any(|dir| std::path::Path::new(dir).join(name).is_file())
}
