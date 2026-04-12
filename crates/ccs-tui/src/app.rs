use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::ui;

#[derive(Debug, Clone)]
pub enum Screen {
    Picker,
    Viewer { live: bool, session: Option<String> },
}

pub struct App {
    screen: Screen,
    should_quit: bool,
}

impl App {
    pub fn new(screen: Screen) -> Self {
        Self {
            screen,
            should_quit: false,
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(screen = ?self.screen))]
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        tracing::info!("entering tui run loop");
        while !self.should_quit {
            terminal.draw(|frame| match &self.screen {
                Screen::Picker => ui::picker::render(frame),
                Screen::Viewer { live, session } => {
                    ui::viewer::render(frame, *live, session.as_deref())
                }
            })?;
            self.handle_event()?;
        }
        Ok(())
    }

    fn handle_event(&mut self) -> Result<()> {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                return Ok(());
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                _ => {}
            }
        }
        Ok(())
    }
}
