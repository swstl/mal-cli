use crate::app::{Action, Event, ExtraInfo};
use crate::mal::MalClient;
use crate::mal::models::anime::{Anime, AnimeId};
use crate::screens::widgets::sync;
use std::collections::HashMap;
use ratatui::layout::{Alignment, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use std::thread::JoinHandle;
use widgets::navbar;
use widgets::popup;
use ratatui::Frame;
use std::any::Any;
use screens::*;

// Below these dimensions some screens (the profile layout in particular) compute
// areas that fall outside the terminal buffer, which makes ratatui panic. When the
// window is smaller than this we render a notice instead of drawing any screen.
const MIN_TERMINAL_WIDTH: u16 = 70;
const MIN_TERMINAL_HEIGHT: u16 = 30;

// Renders a centered "terminal too small" notice. Kept deliberately simple: it only
// ever draws a wrapped paragraph into a sub-rect of `area`, so it cannot itself
// overflow the buffer no matter how tiny the terminal gets.
fn render_terminal_too_small(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);

    let lines = vec![
        Line::from("Terminal too small"),
        Line::from(format!(
            "{}x{} — need at least {}x{}",
            area.width, area.height, MIN_TERMINAL_WIDTH, MIN_TERMINAL_HEIGHT
        )),
    ];

    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    // vertically center the two lines while staying inside the buffer
    let y_offset = area.height.saturating_sub(2) / 2;
    let text_area = Rect {
        x: area.x,
        y: area.y + y_offset,
        width: area.width,
        height: area.height - y_offset,
    };

    frame.render_widget(paragraph, text_area);
}

#[allow(non_snake_case)]
mod screenTemplate;
mod settings;
mod overview;
mod seasons;
mod widgets;
mod profile;
mod launch;
mod search;
mod login;
mod info;
mod list;
mod related;

// this is a macro to define screens in a more structured way
// it allows for screens to be implemented in a single place and work across the app
macro_rules! define_screens {

    // screens provided like bellow:
    // SCREEN1 => "Screen1" => <module>::<structName>,
    ($($name:ident => $display:literal => $module:ident::$struct:ident),* $(,)?) => {
        // this is a module with a const of all available screens
        pub mod screens {
            $(
                pub const $name: &str = concat!($display, "Screen");
            )*
        }

        // this function gives a screen based on its name
        pub fn name_to_screen(screen_name: &str) -> &'static str {
            match screen_name {
                $(
                    $display => screens::$name,
                )*
                _ => screens::LAUNCH,
            }
        }

        // this function returns a display name for the screen
        pub fn screen_to_name(screen_name: &str) -> &str {
            match screen_name {
                $(
                    screens::$name => $display,
                )*
                _ => screen_name.strip_suffix("Screen").unwrap_or(screen_name),
            }
        }

        // this function creates a new screen based on its name
        pub fn create_screen(screen_name: &str, app_info: &ExtraInfo) -> Box<dyn Screen> {
            match screen_name {
                $(
                    screens::$name => Box::new($module::$struct::new(app_info.clone())),
                )*
                _ => Box::new(launch::LaunchScreen::new(app_info.clone())),
            }
        }
    };
}


#[macro_export]
macro_rules! add_screen_caching {
    () => {
        fn should_store(&self) -> bool {
            true
        }

        fn clone_box(&self) -> Box<dyn Screen> {
            Box::new(self.clone())
        }
    };
}
#[macro_export]
macro_rules! check_for_account {
    () => {
        fn needs_accound(&self) -> bool {
            true
        }
    };
}





// INFO: make these screens structs and implement the trait for them.
// INFO: they should take care of their own buttons and such
// when adding new screens add them in define_screens then just call change_screen when you need to draw them
// INFO: Now the screen states are being stored in a hashmap, this could be changed to another
// structure (idk whats best, research this in the future, not important now)
// this could be moved to a screen manager
// but the main focus now is just implementing the screens and making them work
// and then connecting the login functionality to the app
// then use the rest of the mal api
// then start inspecting tachyonfx
// INFO: here:
// INFO: Variable => Display name => Screen struct
define_screens! {
    LAUNCH => "Launch" => launch::LaunchScreen,
    INFO => "Info" => info::InfoScreen,
    OVERVIEW => "Overview" => overview::OverviewScreen,
    SETTINGS => "Settings" => settings::SettingsScreen,
    LOGIN => "Login" => login::LoginScreen,
    PROFILE => "Profile" => profile::ProfileScreen,
    SEASONS => "Seasons" => seasons::SeasonsScreen,
    SEARCH => "Search" => search::SearchScreen,
    LIST => "List" => list::ListScreen,
    RELATED => "Related" => related::RelatedScreen,

    // To add more::
    // SCREEN1 => "<structName>" => <module>::<structName>Screen,
    // SCREEN2 => "<structName>" => <module>::<structName>Screen,
    // etc...
}


#[allow(dead_code, unused_variables)]
pub trait Screen {
    fn draw(&mut self, frame: &mut Frame);
    fn handle_keyboard(&mut self, key_event: crossterm::event::KeyEvent) -> Option<Action> {
        None
    }
    fn handle_mouse(&mut self, mouse_event: crossterm::event::MouseEvent) -> Option<Action> {
        None
    }
    // the name the screen is stored under
    fn get_name(&self) -> String {
        let name = std::any::type_name::<Self>();
        name.split("::").last().unwrap_or(name).to_string()
    }
    fn clone_box(&self) -> Box<dyn Screen> {
        panic!(
            "Attempted to clone a screen type that doesn't support cloning: {}",
            self.get_name()
        );
    }
    fn should_store(&self) -> bool {
       false 
    }
    fn uses_navbar(&self) -> bool {
        true
    }
    fn needs_accound(&self) -> bool {
        false
    }

    //INFO: just create a backgground function that returns a JoinHandle and the screen will have
    //background functionality. Use apply update to pass updates to the rendering thread
    fn background(&mut self) -> Option<JoinHandle<()>> {
        None
    }
    fn apply_update(&mut self, update: BackgroundUpdate) {}

    // hook for screens that can be opened pre-loaded with a specific anime
    // (the Related screen builds its timeline from it). Default: ignore.
    fn build_related(&mut self, _anime: AnimeId) {}
}

pub struct ScreenManager {
    navbar: navbar::NavBar,
    overlay: popup::AnimePopup,
    error_overlay: popup::ErrorPopup,
    current_screen: Box<dyn Screen>,
    screen_storage: HashMap<String, Box<dyn Screen>>,
    backgrounds: Vec<JoinHandle<()>>,
    passable_info: ExtraInfo,
    pub syncing_popup: sync::SyncPopup,
}

#[allow(dead_code)]
impl ScreenManager {
    pub fn new(passable_info: ExtraInfo) -> Self {

        let mut manager = Self {
            // default screen is the launch screen
            navbar: navbar::NavBar::new()
                .add_screen(OVERVIEW)
                .add_screen(SEASONS)
                .add_screen(SEARCH)
                .add_screen(LIST)
                .add_screen(RELATED)
                .add_screen(PROFILE),
            overlay: popup::AnimePopup::new(passable_info.clone()),
            error_overlay: popup::ErrorPopup::new(),
            current_screen: Box::new(launch::LaunchScreen::new(passable_info.clone())),
            screen_storage: HashMap::new(),
            backgrounds: Vec::new(),
            syncing_popup: sync::SyncPopup::new(passable_info.clone()),
            passable_info,
        };

        manager.spawn_background();
        manager
    }

    pub fn render_screen(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < MIN_TERMINAL_WIDTH || area.height < MIN_TERMINAL_HEIGHT {
            render_terminal_too_small(frame, area);
            return;
        }

        self.current_screen.draw(frame);
        if self.current_screen.uses_navbar() {
            let nav_bar_area = Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(3),
                    ratatui::layout::Constraint::Fill(1),
                ])
                .split(frame.area())[0];
            self.navbar.render(frame, nav_bar_area);
        }
        self.overlay.render(frame);
        self.syncing_popup.render(frame);
        self.error_overlay.render(frame);
    }

    pub fn toggle_navbar(&mut self, select: bool) {
        if select {
            self.navbar.select();
        } else {
            self.navbar.deselect();
        }
    }

    pub fn toggle_overlay(&mut self, anime: AnimeId) {
        self.overlay.set_anime(anime);
        self.overlay.open();
    }

    // switch to the Related screen and immediately build its timeline from the
    // given anime (used by the "Related series" button in the anime popup)
    pub fn show_related(&mut self, anime: AnimeId) {
        self.overlay.close();
        self.change_screen(RELATED);
        self.current_screen.build_related(anime);
    }

    pub fn refresh(&mut self) {
        self.overlay.update_buttons();
    }

    pub fn show_error(&mut self, error: String) {
        crate::utils::errorBus::log_to_file(&error);
        self.error_overlay.set_error(error);
        self.error_overlay.open();
    }

    pub fn interactive_sync(&mut self, anime: Vec<Anime>) {
        self.syncing_popup.set_animes(anime);
        self.syncing_popup.open();
    }

    pub fn handle_input(&mut self, event: crossterm::event::Event) -> Option<Action> {
        match event {
            crossterm::event::Event::Key(key_event) => {
                if self.error_overlay.is_open() {
                    return self.error_overlay.handle_keyboard(key_event);
                }

                if self.syncing_popup.is_open() {
                    return self.syncing_popup.handle_keyboard(key_event);
                }

                if self.overlay.is_open() {
                    return self.overlay.handle_keyboard(key_event);
                }

                // global tab navigation: cycle between screens from anywhere
                if self.current_screen.uses_navbar() {
                    let nav = &crate::config::Config::global().navigation;
                    if nav.is_tab_next(&key_event.code) {
                        return self.navbar.cycle(true).map(Action::SwitchScreen);
                    }
                    if nav.is_tab_prev(&key_event.code) {
                        return self.navbar.cycle(false).map(Action::SwitchScreen);
                    }
                }

                if self.navbar.is_selected() {
                    return self.navbar.handle_keyboard(key_event)
                        .and_then(|action| match action {
                        Action::NavbarSelect(_) => self.current_screen.handle_keyboard(key_event),
                        other => Some(other),
                    });
                }

                self.current_screen.handle_keyboard(key_event)
            }

            crossterm::event::Event::Mouse(mouse_event) => {
                if self.error_overlay.is_open() {
                    return self.error_overlay.handle_mouse(mouse_event);
                }

                if self.syncing_popup.is_open() {
                    return self.syncing_popup.handle_mouse(mouse_event);
                }

                if self.overlay.is_open() {
                    return self.overlay.handle_mouse(mouse_event);
                }

                if self.navbar.is_selected() {
                    return self.navbar.handle_mouse(mouse_event)
                        .and_then(|action| match action {
                            Action::NavbarSelect(_) => self.current_screen.handle_mouse(mouse_event),
                            other => Some(other),
                        });
                }

                self.current_screen.handle_mouse(mouse_event)
            }

            _ => { None }
        }

    }

    pub fn update_screen(&mut self, update: BackgroundUpdate) {
        if update.id == "popup" {
            self.overlay.apply_update(update);
            return;
        }

        if self.current_screen.get_name() == update.id {
            self.current_screen.apply_update(update);
        } else if let Some(screen) = self.screen_storage.get_mut(&update.id) {
            screen.apply_update(update);
        }
    }

    // change screen stores the previous screen if not specified otherwise
    // the current screen is removed from the storage if it exists, or created anew
    // this allows for screens to be swapped and their state to be preserved
    pub fn change_screen(&mut self, screen_name: &str) {
        if self.current_screen.should_store() {
            self.screen_storage.insert(
                self.current_screen.get_name(),
                self.current_screen.clone_box(),
            );
        }

        if let Some(screen) = self.screen_storage.remove(screen_name) {
            self.current_screen = screen;
        } else {
            let new_screen = create_screen(screen_name, &self.passable_info);
            if new_screen.needs_accound() && !MalClient::user_is_logged_in() {
                self.show_error("You need to be logged in to access this tab.".to_string());
                return;
            } else {
                self.current_screen = new_screen;
            }
        }

        self.cleanup_backgrounds();
        self.spawn_background();
    }

    pub fn spawn_background(&mut self) {
        if let Some(handle) = self.current_screen.background() {
            self.backgrounds.push(handle);
        }
    }

    // this stops all background threads and waits for them to finish
    pub fn stop_background(&mut self) {
        for handle in self.backgrounds.drain(..) {
            handle.join().unwrap();
        }
    }

    // this cleans up the backgrounds by removing those that are finished
    pub fn cleanup_backgrounds(&mut self) {
        self.backgrounds.retain(|handle| !handle.is_finished());
    }
}

#[derive(Debug)]
pub struct BackgroundUpdate {
    pub id: String,
    pub updates: HashMap<String, Box<dyn Any + Send + Sync>>,
}

#[allow(dead_code)]
impl BackgroundUpdate {
    pub fn new<S: Into<String>>(screen_id: S) -> Self {
        Self {
            id: screen_id.into(),
            updates: HashMap::new(),
        }
    }

    pub fn set<T: Any + Send + Sync, S: Into<String>>(mut self, field: S, value: T) -> Self {
        self.updates.insert(field.into(), Box::new(value));
        self
    }

    pub fn get<T: Any>(&self, field: &str) -> Option<&T> {
        self.updates.get(field)?.downcast_ref::<T>()
    }

    pub fn has(&self, field: &str) -> bool {
        self.updates.contains_key(field)
    }

    pub fn fields(&self) -> impl Iterator<Item = &String> {
        self.updates.keys()
    }

    pub fn take<T: Any + Send + Sync>(&mut self, field: &str) -> Option<T> {
        self.updates
            .remove(field)?
            .downcast::<T>()
            .ok()
            .map(|boxed| *boxed)
    }
}
