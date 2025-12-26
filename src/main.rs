use clap::{Parser, Subcommand};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use indexmap::IndexSet;
use oxidrop::{Endpoint, Oxidrop, TransferRequest};
use ratatui::{
    DefaultTerminal, Frame,
    layout::Offset,
    style::{Modifier, Style},
    text::Line,
    widgets::{List, ListState},
};
use scopeguard::defer;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_stream::{Stream, StreamExt, wrappers::IntervalStream};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, short)]
    log_level: Option<log::LevelFilter>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Send {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    Receive {},
}

enum AppEvent {
    NewEndpoint(Endpoint),
    NewTransferRequest(TransferRequest),
    Up,
    Down,
    Confirm,
    Quit,
    Resize,
    Tick,
    Error(std::io::Error),
}

struct AppState {
    endpoints: IndexSet<Endpoint>,
    requests: IndexSet<TransferRequest>,
    device_name: String,
    list_state: ListState,
    num_dots: usize,
}

impl AppState {
    fn new(oxidrop: &Oxidrop) -> Self {
        AppState {
            num_dots: 1,
            device_name: oxidrop.device_name(),
            endpoints: IndexSet::new(),
            requests: IndexSet::new(),
            list_state: ListState::default(),
        }
    }
}

fn get_input_stream() -> impl Stream<Item = AppEvent> {
    EventStream::new().filter_map(|e| match e {
        Ok(Event::Key(KeyEvent {
            code, modifiers, ..
        })) => match (code, modifiers) {
            (KeyCode::Up | KeyCode::Char('k'), _) => Some(AppEvent::Up),
            (KeyCode::Down | KeyCode::Char('j'), _) => Some(AppEvent::Down),
            (KeyCode::Enter, _) => Some(AppEvent::Confirm),
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                Some(AppEvent::Quit)
            }
            _ => None,
        },
        Ok(Event::Resize(_, _)) => Some(AppEvent::Resize),
        Err(e) => Some(AppEvent::Error(e)),
        _ => None,
    })
}

fn get_interval_stream() -> impl Stream<Item = AppEvent> {
    IntervalStream::new(tokio::time::interval(Duration::from_secs(1))).map(|_| AppEvent::Tick)
}

fn render_send(
    AppState {
        endpoints,
        list_state,
        num_dots,
        ..
    }: &mut AppState,
    frame: &mut Frame,
) {
    let title = Line::from("Select: <￪>/<￬>/<J>/<K>  Send Files: <Enter>  Quit: <Q>/<Ctrl-C>")
        .centered()
        .style(Style::new().add_modifier(Modifier::UNDERLINED));
    let mut area = frame.area();
    frame.render_widget(title, area);

    area = area.offset(Offset { x: 0, y: 1 });
    if endpoints.is_empty() {
        frame.render_widget(
            Line::from(format!("Discovering devices{}", ".".repeat(*num_dots))),
            area,
        );
    } else {
        let list = List::new(endpoints.iter().map(Endpoint::name))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, list_state);
    }
}

async fn do_send(
    oxidrop: Oxidrop,
    term: Arc<Mutex<DefaultTerminal>>,
    files: Vec<PathBuf>,
) -> anyhow::Result<()> {
    let mut state = AppState::new(&oxidrop);
    let mut stream = oxidrop
        .discover_endpoints()?
        .map(|e| AppEvent::NewEndpoint(e))
        .merge(get_input_stream())
        .merge(get_interval_stream());

    while let Some(ev) = stream.next().await {
        match ev {
            AppEvent::NewEndpoint(e) => {
                state.endpoints.insert(e);

                term.lock().unwrap().draw(|f| render_send(&mut state, f))?;
            }
            AppEvent::Up => {
                state.list_state.select_previous();
                term.lock().unwrap().draw(|f| render_send(&mut state, f))?;
            }
            AppEvent::Down => {
                state.list_state.select_next();
                term.lock().unwrap().draw(|f| render_send(&mut state, f))?;
            }
            AppEvent::Confirm => {
                let Some(i) = state.list_state.selected() else {
                    continue;
                };
                let files = files.iter().map(|p| oxidrop::File { path: p.clone() });
                oxidrop.send_files(&state.endpoints[i], files).await?;
            }
            AppEvent::Quit => {
                break;
            }
            AppEvent::Resize => {
                term.lock().unwrap().draw(|f| render_send(&mut state, f))?;
            }
            AppEvent::Tick => {
                state.num_dots = state.num_dots % 3 + 1;
                term.lock().unwrap().draw(|f| render_send(&mut state, f))?;
            }
            AppEvent::Error(e) => Err(e)?,
            AppEvent::NewTransferRequest(_) => unreachable!(),
        }
    }

    Ok(())
}

fn render_receive(
    AppState {
        requests,
        device_name,
        list_state,
        num_dots,
        ..
    }: &mut AppState,
    frame: &mut Frame,
) {
    let title = Line::from("Select: <￪>/<￬>/<J>/<K>  Accept Transfer: <Enter>  Quit: <Q>/<Ctrl-C>")
        .centered()
        .style(Style::new().add_modifier(Modifier::UNDERLINED));
    let mut area = frame.area();
    frame.render_widget(title, area);

    area = area.offset(Offset { x: 0, y: 1 });
    if requests.is_empty() {
        let prompt = Line::from(format!(
            "This deivce will be shown as {}{}",
            device_name,
            ".".repeat(*num_dots)
        ));
        frame.render_widget(prompt, area);
    } else {
        let list = List::new(requests.iter().map(TransferRequest::sender_name))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, list_state);
    }
}

async fn do_receive(oxidrop: Oxidrop, term: Arc<Mutex<DefaultTerminal>>) -> anyhow::Result<()> {
    let mut state = AppState::new(&oxidrop);
    let mut stream = oxidrop
        .get_transfer_requests()?
        .map(|r| AppEvent::NewTransferRequest(r))
        .merge(get_input_stream())
        .merge(get_interval_stream());

    while let Some(ev) = stream.next().await {
        match ev {
            AppEvent::NewTransferRequest(req) => {
                state.requests.insert(req);
                term.lock()
                    .unwrap()
                    .draw(|f| render_receive(&mut state, f))?;
            }
            AppEvent::Up => {
                state.list_state.select_previous();
                term.lock()
                    .unwrap()
                    .draw(|f| render_receive(&mut state, f))?;
            }
            AppEvent::Down => {
                state.list_state.select_next();
                term.lock()
                    .unwrap()
                    .draw(|f| render_receive(&mut state, f))?;
            }
            AppEvent::Confirm => {
                let Some(i) = state.list_state.selected() else {
                    continue;
                };
                oxidrop.accept_transfer(&state.requests[i]).await?;
            }
            AppEvent::Quit => break,
            AppEvent::Resize => {
                term.lock()
                    .unwrap()
                    .draw(|f| render_receive(&mut state, f))?;
            }
            AppEvent::Tick => {
                state.num_dots = state.num_dots % 3 + 1;
                term.lock()
                    .unwrap()
                    .draw(|f| render_receive(&mut state, f))?;
            }
            AppEvent::Error(e) => Err(e)?,
            AppEvent::NewEndpoint(_) => unreachable!(),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut log_cfg = env_logger::builder();
    if let Some(log_level) = cli.log_level {
        log_cfg.filter_level(log_level);
    }
    log_cfg.init();

    let term = Arc::new(Mutex::new(ratatui::init_with_options(
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(10),
        },
    )));
    defer! {
        ratatui::restore();
    }

    let oxidrop = Oxidrop::new(oxidrop::Config { port: Some(9300) }).await?;
    match cli.command {
        Commands::Send { files } => do_send(oxidrop, term, files).await?,
        Commands::Receive {} => do_receive(oxidrop, term).await?,
    }

    Ok(())
}
