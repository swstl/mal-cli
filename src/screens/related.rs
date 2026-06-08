use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::symbols::border;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};

use crate::add_screen_caching;
use crate::app::{Action, Event};
use crate::config::Config;
use crate::config::navigation::NavDirection;
use crate::mal::models::anime::{Anime, AnimeId};
use crate::utils::imageManager::ImageManager;
use crate::utils::input::Input;

use super::widgets::animebox::AnimeBox;
use crate::screens::Screen;

use super::{BackgroundUpdate, ExtraInfo};

// Cap how far the prequel/sequel chain is followed, and a higher overall cap
// including branch nodes that get pre-fetched so selecting them opens the popup.
const LINE_CAP: usize = 25;
const TOTAL_FETCH_CAP: usize = 45;

// Relation types that form the horizontal "watch order" line.
const PREQUEL: &str = "prequel";
const SEQUEL: &str = "sequel";

#[derive(Clone, PartialEq)]
enum Mode {
    Search,
    Picking,
    Timeline,
}

#[derive(Clone, PartialEq)]
enum Focus {
    NavBar,
    Search,
    Results,
    Timeline,
}

#[derive(Clone)]
struct BranchEntry {
    relation_label: String,
    id: AnimeId,
    title: String,
}

#[derive(Clone)]
struct Timeline {
    line: Vec<AnimeId>,
    root_index: usize,
    branches: HashMap<AnimeId, Vec<BranchEntry>>,
}

#[derive(Clone)]
enum LocalEvent {
    Search(String),
    Build(AnimeId),
}

#[derive(Clone)]
pub struct RelatedScreen {
    app_info: ExtraInfo,
    image_manager: Arc<Mutex<ImageManager>>,

    mode: Mode,
    focus: Focus,
    status: Option<String>,
    busy: bool,

    search_input: Input,
    search_area: Option<Rect>,

    results: Vec<AnimeId>,
    pick_index: usize,
    pick_areas: Vec<(usize, Rect)>,

    timeline: Option<Timeline>,
    line_index: usize,
    in_branches: bool,
    branch_index: usize,
    scroll: usize,
    node_areas: Vec<(usize, Rect)>,
    branch_areas: Vec<(usize, Rect)>,

    bg_sender: Option<Sender<LocalEvent>>,
    bg_loaded: bool,
}

impl RelatedScreen {
    pub fn new(info: ExtraInfo) -> Self {
        Self {
            app_info: info,
            image_manager: Arc::new(Mutex::new(ImageManager::new())),
            mode: Mode::Search,
            focus: Focus::Search,
            status: None,
            busy: false,
            search_input: Input::new(),
            search_area: None,
            results: Vec::new(),
            pick_index: 0,
            pick_areas: Vec::new(),
            timeline: None,
            line_index: 0,
            in_branches: false,
            branch_index: 0,
            scroll: 0,
            node_areas: Vec::new(),
            branch_areas: Vec::new(),
            bg_sender: None,
            bg_loaded: false,
        }
    }

    fn send(&self, event: LocalEvent) {
        if let Some(sender) = &self.bg_sender {
            sender.send(event).ok();
        }
    }

    // switch to the timeline page immediately (showing just the searched anime
    // plus a loading placeholder) and kick off the background chain fetch
    fn start_build(&mut self, root_id: AnimeId) {
        self.busy = true;
        self.status = None;
        self.mode = Mode::Timeline;
        self.focus = Focus::Timeline;
        self.line_index = 0;
        self.scroll = 0;
        self.in_branches = false;
        self.branch_index = 0;
        self.timeline = Some(Timeline {
            line: vec![root_id],
            root_index: 0,
            branches: HashMap::new(),
        });
        self.send(LocalEvent::Build(root_id));
    }

    // move focus from the search box into whatever content is showing
    fn leave_search(&mut self) {
        if self.mode == Mode::Picking && !self.results.is_empty() {
            self.focus = Focus::Results;
        } else if self.mode == Mode::Timeline {
            self.focus = Focus::Timeline;
        }
    }

    // current branch list for the selected line node, if any
    fn current_branches(&self) -> &[BranchEntry] {
        self.timeline
            .as_ref()
            .and_then(|t| t.line.get(self.line_index).and_then(|id| t.branches.get(id)))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

// Walk the relation graph outward from `root` to build the watch-order line and
// collect side relations as branches. `fetch` resolves an anime id to a fully
// populated Anime (it carries its own `related_anime`). Pure over `fetch` so it
// can be unit-tested with a synthetic graph.
fn build_timeline<F, E, B>(
    root: Anime,
    mut fetch: F,
    mut emit: E,
    mut emit_branch: B,
) -> (Vec<Anime>, Vec<AnimeId>, usize, HashMap<AnimeId, Vec<BranchEntry>>)
where
    F: FnMut(AnimeId) -> Option<Anime>,
    // called as each line node is fetched: (new anime, current ordered line, root index)
    E: FnMut(&Anime, Vec<AnimeId>, usize),
    // called as each branch is resolved: (parent line-node id, entry, fetched anime if any)
    B: FnMut(AnimeId, &BranchEntry, Option<&Anime>),
{
    let root_id = root.id;
    let mut visited: HashSet<AnimeId> = HashSet::new();
    visited.insert(root_id);

    let mut animes: Vec<Anime> = Vec::new();

    // walk backward along prequels, emitting each as it arrives
    let mut back: Vec<Anime> = Vec::new();
    let mut node = root.clone();
    while back.len() + 1 < LINE_CAP {
        let Some(next_id) = first_relation(&node, PREQUEL, &visited) else { break };
        let Some(fetched) = fetch(next_id) else { break };
        visited.insert(fetched.id);
        node = fetched.clone();
        back.push(fetched.clone());

        let mut line: Vec<AnimeId> = back.iter().rev().map(|a| a.id).collect();
        line.push(root_id);
        emit(&fetched, line, back.len());
    }

    // walk forward along sequels, emitting each as it arrives
    let mut forward: Vec<Anime> = Vec::new();
    let mut node = root.clone();
    while back.len() + forward.len() + 1 < LINE_CAP {
        let Some(next_id) = first_relation(&node, SEQUEL, &visited) else { break };
        let Some(fetched) = fetch(next_id) else { break };
        visited.insert(fetched.id);
        node = fetched.clone();
        forward.push(fetched.clone());

        let mut line: Vec<AnimeId> = back.iter().rev().map(|a| a.id).collect();
        line.push(root_id);
        line.extend(forward.iter().map(|a| a.id));
        emit(&fetched, line, back.len());
    }

    // assemble the ordered line: reversed prequels .. root .. sequels
    let root_index = back.len();
    let mut line_animes: Vec<Anime> = Vec::with_capacity(back.len() + 1 + forward.len());
    line_animes.extend(back.into_iter().rev());
    line_animes.push(root);
    line_animes.extend(forward);

    let line: Vec<AnimeId> = line_animes.iter().map(|a| a.id).collect();
    let line_ids: HashSet<AnimeId> = line.iter().copied().collect();

    // branches: every related anime of a line node that isn't itself on the line.
    // Each branch's anime is fetched (so selecting it can open the popup) and
    // emitted one at a time so side stories pop in as they arrive, just like the
    // main line. A failed/skipped fetch still emits the entry (title only).
    let mut branches: HashMap<AnimeId, Vec<BranchEntry>> = HashMap::new();
    for a in &line_animes {
        for rel in a.related_anime.iter().flatten() {
            let rid = rel.node.id as AnimeId;
            if line_ids.contains(&rid) {
                continue;
            }
            let entry = BranchEntry {
                relation_label: rel.relation_type_formatted.clone(),
                id: rid,
                title: rel.node.title.clone(),
            };

            let fetched = if !visited.contains(&rid)
                && line_animes.len() + animes.len() < TOTAL_FETCH_CAP
            {
                fetch(rid)
            } else {
                None
            };
            if let Some(f) = &fetched {
                visited.insert(f.id);
            }

            emit_branch(a.id, &entry, fetched.as_ref());

            if let Some(f) = fetched {
                animes.push(f);
            }
            branches.entry(a.id).or_default().push(entry);
        }
    }

    animes.extend(line_animes);

    (animes, line, root_index, branches)
}

fn first_relation(anime: &Anime, rel_type: &str, visited: &HashSet<AnimeId>) -> Option<AnimeId> {
    anime
        .related_anime
        .iter()
        .flatten()
        .find(|r| r.relation_type == rel_type && !visited.contains(&(r.node.id as AnimeId)))
        .map(|r| r.node.id as AnimeId)
}

impl Screen for RelatedScreen {
    add_screen_caching!();

    // entry point used by the "Related series" popup button: jump straight to a
    // timeline built from the given anime, skipping search/picking
    fn build_related(&mut self, anime: AnimeId) {
        self.results.clear();
        self.start_build(anime);
    }

    fn background(&mut self) -> Option<JoinHandle<()>> {
        if self.bg_loaded {
            return None;
        }
        self.bg_loaded = true;

        let (tx, rx) = channel::<LocalEvent>();
        self.bg_sender = Some(tx);
        let id = self.get_name();
        let mal_client = self.app_info.mal_client.clone();
        let app_sx = self.app_info.app_sx.clone();
        ImageManager::init_with_threads(&self.image_manager, app_sx.clone());

        Some(std::thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    LocalEvent::Search(query) => {
                        let results = mal_client.search_anime(query, 0, 30).unwrap_or_default();
                        let ids: Vec<AnimeId> = results.iter().map(|a| a.id).collect();
                        let update = BackgroundUpdate::new(id.clone())
                            .set("animes", results)
                            .set("search_results", ids);
                        app_sx.send(Event::BackgroundNotice(update)).ok();
                    }
                    LocalEvent::Build(root_id) => {
                        match mal_client.get_anime_by_id(root_id as u64) {
                            Some(root) => {
                                // stream each node to the screen as it's fetched
                                let emit = |anime: &Anime, line: Vec<AnimeId>, root_index: usize| {
                                    let update = BackgroundUpdate::new(id.clone())
                                        .set("animes", vec![anime.clone()])
                                        .set("timeline_line", line)
                                        .set("timeline_root", root_index)
                                        .set("building", true);
                                    app_sx.send(Event::BackgroundNotice(update)).ok();
                                };
                                let emit_branch =
                                    |parent: AnimeId, entry: &BranchEntry, anime: Option<&Anime>| {
                                        let mut update = BackgroundUpdate::new(id.clone())
                                            .set("branch_for", parent)
                                            .set("branch_entry", entry.clone())
                                            .set("building", true);
                                        if let Some(a) = anime {
                                            update = update.set("animes", vec![a.clone()]);
                                        }
                                        app_sx.send(Event::BackgroundNotice(update)).ok();
                                    };
                                let (animes, line, root_index, branches) = build_timeline(
                                    root,
                                    |id| mal_client.get_anime_by_id(id as u64),
                                    emit,
                                    emit_branch,
                                );
                                // final update: full line + branches, building done
                                let update = BackgroundUpdate::new(id.clone())
                                    .set("animes", animes)
                                    .set("timeline_line", line)
                                    .set("timeline_root", root_index)
                                    .set("timeline_branches", branches)
                                    .set("building", false);
                                app_sx.send(Event::BackgroundNotice(update)).ok();
                            }
                            None => {
                                let update = BackgroundUpdate::new(id.clone())
                                    .set("build_failed", true);
                                app_sx.send(Event::BackgroundNotice(update)).ok();
                            }
                        }
                    }
                }
            }
        }))
    }

    fn apply_update(&mut self, mut update: BackgroundUpdate) {
        if let Some(results) = update.take::<Vec<AnimeId>>("search_results") {
            self.busy = false;
            self.results = results;
            self.pick_index = 0;
            if self.results.is_empty() {
                self.status = Some("No results found".to_string());
                self.mode = Mode::Search;
                self.focus = Focus::Search;
            } else {
                self.status = None;
                self.mode = Mode::Picking;
                self.focus = Focus::Results;
            }
        }

        if let Some(line) = update.take::<Vec<AnimeId>>("timeline_line") {
            let root_index = update.take::<usize>("timeline_root").unwrap_or(0);
            let building = update.take::<bool>("building").unwrap_or(false);
            let branches = update.take::<HashMap<AnimeId, Vec<BranchEntry>>>("timeline_branches");

            // merge the incremental update into the (already shown) timeline
            let timeline = self.timeline.get_or_insert_with(|| Timeline {
                line: Vec::new(),
                root_index: 0,
                branches: HashMap::new(),
            });
            timeline.line = line;
            timeline.root_index = root_index;
            if let Some(branches) = branches {
                timeline.branches = branches;
            }
            self.busy = building;
            self.status = None;
            self.mode = Mode::Timeline;
            // keep the highlight on the searched anime as boxes stream in
            self.line_index = root_index;
        }

        // a single branch (side story etc.) streamed in for a line node
        if let Some(parent) = update.take::<AnimeId>("branch_for")
            && let Some(entry) = update.take::<BranchEntry>("branch_entry")
            && let Some(timeline) = self.timeline.as_mut()
        {
            timeline.branches.entry(parent).or_default().push(entry);
        }

        if update.take::<bool>("build_failed").is_some() {
            // keep whatever we already show (the searched anime) and just stop
            // the loading indicator rather than bouncing back to the list
            self.busy = false;
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        // leave room for the navbar at the top (rendered by the manager)
        let [_navbar, body] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Fill(1)])
            .areas(area);

        let [search_area, content] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Fill(1)])
            .areas(body);

        self.draw_search_bar(frame, search_area);

        match self.mode {
            Mode::Search => self.draw_hint(frame, content),
            Mode::Picking => self.draw_picking(frame, content),
            Mode::Timeline => self.draw_timeline(frame, content),
        }
    }

    fn handle_keyboard(&mut self, key_event: crossterm::event::KeyEvent) -> Option<Action> {
        let nav = &Config::global().navigation;
        let modifier = key_event
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);

        // ctrl+up steps focus up one level (timeline/results -> search -> navbar)
        // so a plain up never yanks you out of the content unexpectedly
        if modifier && nav.get_direction(&key_event.code) == NavDirection::Up {
            match self.focus {
                Focus::Timeline => {
                    self.in_branches = false;
                    self.focus = Focus::Search;
                }
                Focus::Results => self.focus = Focus::Search,
                _ => {
                    self.focus = Focus::NavBar;
                    return Some(Action::NavbarSelect(true));
                }
            }
            return None;
        }

        match self.focus {
            Focus::NavBar => {
                self.focus = Focus::Search;
                None
            }
            Focus::Search => {
                // Ctrl + down moves out of the search box; without the modifier
                // the key (e.g. "j") is typed into the query instead.
                if modifier && nav.get_direction(&key_event.code) == NavDirection::Down {
                    self.leave_search();
                } else if let Some(query) = self.search_input.handle_event(key_event, false) {
                    self.busy = true;
                    self.status = Some("Searching…".to_string());
                    self.send(LocalEvent::Search(query));
                }
                None
            }
            Focus::Results => self.handle_picking_keys(nav, &key_event),
            Focus::Timeline => self.handle_timeline_keys(nav, &key_event),
        }
    }

    fn handle_mouse(&mut self, mouse_event: crossterm::event::MouseEvent) -> Option<Action> {
        if mouse_event.row < 3 {
            self.focus = Focus::NavBar;
            return Some(Action::NavbarSelect(true));
        }
        let pos = Position::new(mouse_event.column, mouse_event.row);

        // hovering / clicking a search result highlights it (and click builds it)
        if self.mode == Mode::Picking {
            if let Some((idx, _)) = self.pick_areas.iter().find(|(_, r)| r.contains(pos)) {
                let idx = *idx;
                self.focus = Focus::Results;
                self.pick_index = idx;
                if let crossterm::event::MouseEventKind::Down(_) = mouse_event.kind
                    && let Some(root_id) = self.results.get(idx).copied()
                {
                    self.start_build(root_id);
                }
                return None;
            }
        }

        // hovering / clicking a timeline box highlights it (and click opens it)
        if self.mode == Mode::Timeline {
            if let Some((node_idx, _)) = self.node_areas.iter().find(|(_, r)| r.contains(pos)) {
                let node_idx = *node_idx;
                self.focus = Focus::Timeline;
                self.in_branches = false;
                self.line_index = node_idx;
                if let crossterm::event::MouseEventKind::Down(_) = mouse_event.kind
                    && let Some(id) = self.timeline.as_ref().and_then(|t| t.line.get(node_idx).copied())
                    && self.app_info.anime_store.get(&id).is_some()
                {
                    return Some(Action::ShowOverlay(id));
                }
                return None;
            }

            // hovering / clicking a related (branch) entry highlights and opens it
            if let Some((bi, _)) = self.branch_areas.iter().find(|(_, r)| r.contains(pos)) {
                let bi = *bi;
                self.focus = Focus::Timeline;
                self.in_branches = true;
                self.branch_index = bi;
                if let crossterm::event::MouseEventKind::Down(_) = mouse_event.kind
                    && let Some(id) = self.current_branches().get(bi).map(|b| b.id)
                    && self.app_info.anime_store.get(&id).is_some()
                {
                    return Some(Action::ShowOverlay(id));
                }
                return None;
            }
        }

        if let Some(search_area) = self.search_area {
            if search_area.contains(pos) {
                self.focus = Focus::Search;
            } else if mouse_event.row >= search_area.y + search_area.height {
                // clicking anywhere below the search box moves focus into content
                self.leave_search();
            }
        }
        None
    }
}

impl RelatedScreen {
    fn handle_picking_keys(
        &mut self,
        nav: &crate::config::navigation::Navigation,
        key_event: &crossterm::event::KeyEvent,
    ) -> Option<Action> {
        match nav.get_direction(&key_event.code) {
            NavDirection::Up => {
                if self.pick_index == 0 {
                    self.focus = Focus::Search;
                } else {
                    self.pick_index -= 1;
                }
            }
            NavDirection::Down => {
                if self.pick_index + 1 < self.results.len() {
                    self.pick_index += 1;
                }
            }
            _ => {}
        }

        if nav.is_select(&key_event.code)
            && let Some(root_id) = self.results.get(self.pick_index).copied()
        {
            self.start_build(root_id);
        }
        None
    }

    fn handle_timeline_keys(
        &mut self,
        nav: &crate::config::navigation::Navigation,
        key_event: &crossterm::event::KeyEvent,
    ) -> Option<Action> {
        let line_len = self.timeline.as_ref().map(|t| t.line.len()).unwrap_or(0);
        if line_len == 0 {
            return None;
        }

        match nav.get_direction(&key_event.code) {
            NavDirection::Left => {
                self.in_branches = false;
                if self.line_index > 0 {
                    self.line_index -= 1;
                }
            }
            NavDirection::Right => {
                self.in_branches = false;
                if self.line_index + 1 < line_len {
                    self.line_index += 1;
                }
            }
            NavDirection::Down => {
                let branch_len = self.current_branches().len();
                if !self.in_branches && branch_len > 0 {
                    self.in_branches = true;
                    self.branch_index = 0;
                } else if self.in_branches && self.branch_index + 1 < branch_len {
                    self.branch_index += 1;
                }
            }
            NavDirection::Up => {
                // plain up only navigates within the branch list; leaving the
                // timeline upward is reserved for ctrl+up
                if self.in_branches {
                    if self.branch_index == 0 {
                        self.in_branches = false;
                    } else {
                        self.branch_index -= 1;
                    }
                }
            }
            NavDirection::None => {}
        }

        if nav.is_select(&key_event.code) {
            let target = if self.in_branches {
                self.current_branches().get(self.branch_index).map(|b| b.id)
            } else {
                self.timeline.as_ref().and_then(|t| t.line.get(self.line_index).copied())
            };
            if let Some(id) = target
                && self.app_info.anime_store.get(&id).is_some()
            {
                return Some(Action::ShowOverlay(id));
            }
        }
        None
    }

    fn draw_search_bar(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Search;
        let field = Paragraph::new(self.search_input.value())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Search")
                    .border_set(border::ROUNDED),
            )
            .style(Style::default().fg(if focused {
                Config::global().theme.highlight
            } else {
                Config::global().theme.primary
            }));
        frame.render_widget(field, area);
        self.search_area = Some(area);
        self.search_input
            .render_cursor(frame, area.x + 1, area.y + 1, focused);
    }

    fn draw_hint(&self, frame: &mut Frame, area: Rect) {
        let msg = self
            .status
            .clone()
            .unwrap_or_else(|| "Type an anime title and press Enter to see its timeline.".to_string());
        let p = Paragraph::new(msg)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Config::global().theme.text))
            .block(Block::default().padding(Padding::new(2, 2, 2, 2)));
        frame.render_widget(p, area);
    }

    fn draw_picking(&mut self, frame: &mut Frame, area: Rect) {
        self.pick_areas.clear();
        let theme = &Config::global().theme;

        // header line, then the bordered rows below it
        let [header_area, list_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Fill(1)])
            .areas(area.inner(ratatui::layout::Margin::new(2, 1)));
        frame.render_widget(
            Paragraph::new("Pick the anime to build the timeline from:")
                .style(Style::default().fg(theme.text)),
            header_area,
        );

        const ROW_H: u16 = 6;
        const GAP: u16 = 1;
        let slot = ROW_H + GAP;
        let visible = (list_area.height / slot).max(1) as usize;
        let start = if self.pick_index >= visible {
            self.pick_index + 1 - visible
        } else {
            0
        };

        for (offset, i) in (start..self.results.len()).enumerate() {
            if offset >= visible {
                break;
            }
            let id = self.results[i];
            let Some(anime) = self.app_info.anime_store.get(&id) else { continue };
            let row = Rect::new(
                list_area.x,
                list_area.y + offset as u16 * slot,
                list_area.width,
                ROW_H,
            );

            let selected = i == self.pick_index && self.focus == Focus::Results;
            let border_color = if selected { theme.highlight } else { theme.primary };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(border_color));
            let inner = block.inner(row);
            frame.render_widget(block, row);

            // thumbnail on the left, text on the right
            let [thumb_area, text_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(8), Constraint::Fill(1)])
                .areas(inner);
            ImageManager::render_image(&self.image_manager, &*anime, frame, thumb_area, true);

            let text = Text::from(vec![
                Line::from(Span::styled(
                    anime.display_title(),
                    Style::default().fg(if selected { theme.highlight } else { theme.secondary }),
                )),
                Line::from(Span::styled(
                    format!("{} · {} ep", anime.media_type, anime.num_episodes),
                    Style::default().fg(theme.text),
                )),
            ]);
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .block(Block::default().padding(Padding::new(1, 1, 1, 0))),
                text_area,
            );

            self.pick_areas.push((i, row));
        }
    }

    fn draw_timeline(&mut self, frame: &mut Frame, area: Rect) {
        let Some(timeline) = self.timeline.clone() else { return };

        // the strip is a fixed, compact height (label + box + a little breathing
        // room); the branch panel sits right under it and the rest stays empty
        const STRIP_H: u16 = 15;
        let branch_count = self.current_branches().len() as u16;
        let branch_h = if branch_count == 0 {
            3
        } else {
            (branch_count + 4).min(area.height / 2)
        };
        let [strip_area, branch_area, _rest] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(STRIP_H.min(area.height)),
                Constraint::Length(branch_h),
                Constraint::Fill(1),
            ])
            .areas(area);

        self.draw_strip(frame, strip_area, &timeline);
        self.draw_branches(frame, branch_area, &Config::global().theme);
    }

    // horizontal, scrollable strip of AnimeBoxes, each captioned with its
    // relation to the searched anime (Prequel / Searched / Sequel)
    fn draw_strip(&mut self, frame: &mut Frame, area: Rect, timeline: &Timeline) {
        let theme = &Config::global().theme;
        self.node_areas.clear();
        const NODE_W: u16 = 34;
        const GAP: u16 = 2;
        const BOX_H: u16 = 12;
        let slot = NODE_W + GAP;
        let visible = ((area.width.saturating_sub(2)) / slot).max(1) as usize;

        // a label row + a fixed-height box, vertically centered in the strip
        let cell_h = BOX_H + 1;
        let cell_y = area.y + area.height.saturating_sub(cell_h) / 2;

        // keep the selected node within the visible window
        let mut scroll = self.scroll;
        if self.line_index < scroll {
            scroll = self.line_index;
        } else if self.line_index >= scroll + visible {
            scroll = self.line_index + 1 - visible;
        }
        let end = (scroll + visible).min(timeline.line.len());

        for (slot_idx, node_idx) in (scroll..end).enumerate() {
            let x = area.x + 1 + slot_idx as u16 * slot;
            if x + NODE_W > area.x + area.width {
                break;
            }
            let is_sel = self.focus == Focus::Timeline
                && node_idx == self.line_index
                && !self.in_branches;

            let label_area = Rect::new(x, cell_y, NODE_W, 1);
            let box_area = Rect::new(x, cell_y + 1, NODE_W, BOX_H);

            // relation caption — lights up with the box when selected
            let label = if node_idx == timeline.root_index {
                "● Searched"
            } else if node_idx < timeline.root_index {
                "Prequel"
            } else {
                "Sequel"
            };
            let label_color = if is_sel {
                theme.highlight
            } else if node_idx == timeline.root_index {
                theme.second_highlight
            } else {
                theme.primary
            };
            frame.render_widget(
                Paragraph::new(label)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(label_color)),
                label_area,
            );

            // the anime box itself (highlighted when this node is selected)
            if let Some(anime) = self.app_info.anime_store.get(&timeline.line[node_idx]) {
                AnimeBox::render(&anime, &self.image_manager, frame, box_area, is_sel);
            }
            self.node_areas.push((node_idx, box_area));
        }

        // while the chain is still fetching, show a "Loading…" placeholder box
        // in the next slot so it's clear more is on the way
        if self.busy {
            let slot_idx = (end - scroll) as u16;
            let x = area.x + 1 + slot_idx * slot;
            if x + NODE_W <= area.x + area.width {
                frame.render_widget(
                    Paragraph::new("…")
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(theme.primary)),
                    Rect::new(x, cell_y, NODE_W, 1),
                );
                let box_area = Rect::new(x, cell_y + 1, NODE_W, BOX_H);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_set(border::ROUNDED)
                    .border_style(Style::default().fg(theme.primary));
                let inner = block.inner(box_area);
                frame.render_widget(block, box_area);
                let mid = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1);
                frame.render_widget(
                    Paragraph::new("Loading…")
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(theme.primary)),
                    mid,
                );
            }
        }

        // overflow markers
        let my = cell_y + cell_h / 2;
        if scroll > 0 {
            frame.render_widget(
                Paragraph::new("◀").style(Style::default().fg(theme.primary)),
                Rect::new(area.x, my, 1, 1),
            );
        }
        if end < timeline.line.len() {
            frame.render_widget(
                Paragraph::new("▶").style(Style::default().fg(theme.primary)),
                Rect::new(area.x + area.width - 1, my, 1, 1),
            );
        }
    }

    fn draw_branches(&mut self, frame: &mut Frame, area: Rect, theme: &crate::config::theme::Theme) {
        self.branch_areas.clear();
        let mut lines: Vec<Line> = Vec::new();
        let branches = self.current_branches();
        let count = branches.len();
        if branches.is_empty() {
            lines.push(Line::from(Span::styled(
                "←/→ move · Enter opens · ctrl+↑ search",
                Style::default().fg(theme.primary),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Related:",
                Style::default().fg(theme.text),
            )));
            for (i, b) in branches.iter().enumerate() {
                let selected =
                    self.focus == Focus::Timeline && self.in_branches && i == self.branch_index;
                let marker = if selected { "▸ " } else { "  " };
                let style = if selected {
                    Style::default().fg(theme.highlight)
                } else {
                    Style::default().fg(theme.text)
                };
                lines.push(Line::from(Span::styled(
                    format!("{}{:<19} ▸ {}", marker, b.relation_label, b.title),
                    style,
                )));
            }
            lines.push(Line::from(Span::styled(
                "←/→ move · ↑/↓ branches · Enter opens · ctrl+↑ search",
                Style::default().fg(theme.primary),
            )));
        }

        let panel = Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_set(border::PLAIN)
                    .padding(Padding::new(2, 2, 0, 0)),
            );
        frame.render_widget(panel, area);

        // record clickable rows: top border occupies row 0, "Related:" header
        // row 1, so branch entry `i` sits at row offset 2 + i.
        for i in 0..count {
            let row = area.y + 2 + i as u16;
            if row >= area.y + area.height {
                break;
            }
            self.branch_areas
                .push((i, Rect::new(area.x + 1, row, area.width.saturating_sub(2), 1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mal::models::anime::{Anime, Node, RelatedAnime};

    fn rel(id: u64, kind: &str) -> RelatedAnime {
        RelatedAnime {
            node: Node {
                id,
                title: format!("Anime {}", id),
                main_picture: None,
            },
            relation_type: kind.to_string(),
            relation_type_formatted: kind.to_string(),
        }
    }

    fn anime_with(id: usize, relations: Vec<RelatedAnime>) -> Anime {
        let mut a = Anime::default();
        a.id = id;
        a.title = format!("Anime {}", id);
        a.related_anime = Some(relations);
        a
    }

    // build a fetcher backed by a fixed set of animes
    fn fetcher(animes: Vec<Anime>) -> impl FnMut(AnimeId) -> Option<Anime> {
        let map: HashMap<AnimeId, Anime> = animes.into_iter().map(|a| (a.id, a)).collect();
        move |id| map.get(&id).cloned()
    }

    #[test]
    fn linear_chain_is_ordered() {
        // 1 -> 2 -> 3 (sequels), root is 2
        let a1 = anime_with(1, vec![rel(2, SEQUEL)]);
        let a2 = anime_with(2, vec![rel(1, PREQUEL), rel(3, SEQUEL)]);
        let a3 = anime_with(3, vec![rel(2, PREQUEL)]);
        let f = fetcher(vec![a1.clone(), a2.clone(), a3.clone()]);

        let (_animes, line, root_index, branches) = build_timeline(a2, f, |_, _, _| {}, |_, _, _| {});
        assert_eq!(line, vec![1, 2, 3]);
        assert_eq!(root_index, 1);
        assert!(branches.is_empty());
    }

    #[test]
    fn extra_sequel_becomes_branch() {
        // root 1 has two sequels: 2 (line) and 3 (branch)
        let a1 = anime_with(1, vec![rel(2, SEQUEL), rel(3, SEQUEL)]);
        let a2 = anime_with(2, vec![rel(1, PREQUEL)]);
        let a3 = anime_with(3, vec![rel(1, PREQUEL)]);
        let f = fetcher(vec![a1.clone(), a2.clone(), a3.clone()]);

        let (_a, line, _ri, branches) = build_timeline(a1, f, |_, _, _| {}, |_, _, _| {});
        assert_eq!(line, vec![1, 2]);
        let b = branches.get(&1).expect("node 1 has a branch");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].id, 3);
    }

    #[test]
    fn side_story_is_a_branch_not_on_line() {
        let a1 = anime_with(1, vec![rel(5, "side_story")]);
        let f = fetcher(vec![anime_with(5, vec![])]);
        let (_a, line, _ri, branches) = build_timeline(a1, f, |_, _, _| {}, |_, _, _| {});
        assert_eq!(line, vec![1]);
        assert_eq!(branches.get(&1).unwrap()[0].id, 5);
    }

    #[test]
    fn cycle_terminates() {
        // 1 <-> 2 cyclic sequels
        let a1 = anime_with(1, vec![rel(2, SEQUEL)]);
        let a2 = anime_with(2, vec![rel(1, SEQUEL), rel(1, PREQUEL)]);
        let f = fetcher(vec![a1.clone(), a2.clone()]);
        let (_a, line, _ri, _b) = build_timeline(a1, f, |_, _, _| {}, |_, _, _| {});
        // visited-set prevents revisiting node 1
        assert_eq!(line, vec![1, 2]);
    }

    #[test]
    fn no_relations_single_node() {
        let a1 = anime_with(42, vec![]);
        let f = fetcher(vec![]);
        let (_a, line, root_index, branches) = build_timeline(a1, f, |_, _, _| {}, |_, _, _| {});
        assert_eq!(line, vec![42]);
        assert_eq!(root_index, 0);
        assert!(branches.is_empty());
    }
}
