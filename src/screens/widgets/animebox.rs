use std::sync::{Arc, Mutex};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::Style,
    symbols,
    text::Text,
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
};

use crate::{
    config::Config,
    mal::models::anime::Anime,
    utils::{
        imageManager::ImageManager,
        stringManipulation::{DisplayString, format_date, format_date_short},
    },
};

const FETCH_IMAGE_ON_DEMAND: bool = true;

pub struct AnimeBox {}

impl AnimeBox {
    pub fn render(
        anime: &Anime,
        image_manager: &Arc<Mutex<ImageManager>>,
        frame: &mut Frame,
        area: Rect,
        highlight: bool,
    ) {
        /////////////////////////////////////
        /////// Check if empty anime ////////
        /////////////////////////////////////
        if anime.id == 0 {
            let title = Paragraph::new("")
                .alignment(Alignment::Center)
                .style(Style::default().fg(if highlight {
                    Config::global().theme.highlight
                } else {
                    Config::global().theme.primary
                }))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .padding(Padding::new(1, 1, 1, 1)),
                );
            frame.render_widget(title, area);
            return;
        }

        /////////////////////////////////////
        /////// Determine the colors ////////
        /////////////////////////////////////

        let my_list_color = if highlight {
            Config::global().theme.highlight
        } else if anime.my_list_status.status.is_empty() {
            Config::global().theme.text
        } else {
            Config::global()
                .theme
                .status_color(&anime.my_list_status.status)
        };

        let text_color = Config::global().theme.text;

        let block_color = if highlight {
            Config::global().theme.highlight
        } else {
            Config::global()
                .theme
                .status_color(&anime.my_list_status.status)
        };

        //////////////////////////////////////
        //////// Define the text info ////////
        //////////////////////////////////////
        let title_text = anime.display_title();

        let info_text = "Scr: \nTyp: \nEpi: \nSta: \nSea: ";
        let season = DisplayString::new()
            .add(&anime.start_season.season)
            .capitalize(0)
            .build("{0}");

        let value_text = format!(
            "{}\n{}\n{}\n{}\n{}",
            anime.mean, anime.media_type, anime.num_episodes, anime.status, season
        );

        let airing_text = if anime.start_date == anime.end_date {
            format_date_short(&anime.start_date).to_string()
        } else {
            format!(
                "{}\n->\n{}",
                format_date_short(&anime.start_date),
                format_date_short(&anime.end_date)
            )
        };

        let user_stats_value_text = if anime.my_list_status.score > 0 {
            format!(
                "{} ★{}",
                anime.my_list_status.status, anime.my_list_status.score
            )
        } else {
            anime.my_list_status.status.to_string()
        };

        //////////////////////////////////////
        //////// Color text elements  ////////
        //////////////////////////////////////

        let title_text = Text::styled(title_text, Style::default().fg(text_color));
        let info_text = Text::styled(info_text, Style::default().fg(text_color));
        let value_text = Text::styled(value_text, Style::default().fg(text_color));
        let airing_text = Text::styled(airing_text, Style::default().fg(text_color));
        let user_stats_value_text = Text::styled(user_stats_value_text, Style::default().fg(my_list_color));

        /////////////////////////////////////
        /////// Render the background ///////
        /////////////////////////////////////

        frame.render_widget(
            Block::new()
                .borders(Borders::ALL)
                .border_style(block_color)
                .border_set(symbols::border::ROUNDED),
            area,
        );


        /////////////////////////////////////////
        /////// Split the different areas ///////
        /////////////////////////////////////////

        // title + split into info area
        let [title_area, info_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Fill(1)])
            .areas(area);

        let (info_set, info_borders) = (
            symbols::border::Set {
                top_right: symbols::line::VERTICAL_LEFT,
                top_left: symbols::line::VERTICAL_RIGHT,
                ..symbols::border::ROUNDED
            },
            Borders::ALL,
        );

        let info_block = Block::default()
            .borders(info_borders)
            .border_set(info_set)
            .style(Style::default().fg(block_color));
        frame.render_widget(info_block, info_area);

        let title = Paragraph::new(title_text)
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::new(2, 2, 1, 0)));
        frame.render_widget(title, title_area);

        let [image_area, info_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(info_area);


        //////////////////////////////////
        ///////  Render the image  ///////
        //////////////////////////////////

        let image_area = image_area.inner(Margin::new(1, 1));
        ImageManager::render_image(
            image_manager,
            anime,
            frame,
            image_area,
            FETCH_IMAGE_ON_DEMAND,
        );


        ///////////////////////////////
        /////// Render the text ///////
        ///////////////////////////////

        let [info, value] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Fill(1), Constraint::Fill(1)])
            .areas(info_area);

        let info_paragraph = Paragraph::new(info_text)
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::new(0, 0, 1, 1)));

        let value_paragraph = Paragraph::new(value_text)
            .alignment(Alignment::Left)
            .block(Block::default().padding(Padding::new(0, 1, 1, 1)));

        let airing_paragraph = Paragraph::new(airing_text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::default().padding(Padding::new(0, 2, 8, 1)));

        let [info_area, user_stats_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Fill(1), Constraint::Length(2)])
            .areas(info_area);

        let user_stats_value_paragraph = Paragraph::new(user_stats_value_text)
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::new(0, 2, 0, 1)));

        frame.render_widget(info_paragraph, info);
        frame.render_widget(value_paragraph, value);
        frame.render_widget(airing_paragraph, info_area);
        frame.render_widget(user_stats_value_paragraph, user_stats_area);
    }
}
