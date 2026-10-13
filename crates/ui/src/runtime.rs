//! The terminal loop: draws the app, feeds it keys and background results, and
//! carries out its [`Effect`]s.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use fast_aws_core::{ConfigFactory, Ctx, ListQuery, ResourceKind, Scope, caller_identity};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::crossterm::{ExecutableCommand, cursor};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinSet;

use crate::app::{App, Effect, Msg};
use crate::render;

enum Input {
    Key(event::KeyEvent),
    Redraw,
    Msg(Msg),
}

/// Runs the app until it quits. The terminal is restored on return and on panic.
pub async fn run(
    mut app: App,
    initial: Vec<Effect>,
    factory: Arc<ConfigFactory>,
) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let (tx, mut rx) = unbounded_channel();
    let input = InputThread::spawn(tx.clone());
    let mut loads = JoinSet::new();

    let mut pending = initial;
    'main: loop {
        for effect in std::mem::take(&mut pending) {
            match effect {
                Effect::Quit => break 'main,
                Effect::CancelLoads => loads.abort_all(),
                Effect::LoadIdentity => {
                    let (factory, tx) = (factory.clone(), tx.clone());
                    let (profile, region) = (app.profile.clone(), app.region.clone());
                    loads.spawn(async move {
                        let ctx = factory.ctx(&profile, &region).await;
                        let _ = tx.send(Input::Msg(Msg::Identity(caller_identity(&ctx).await)));
                    });
                }
                Effect::LoadList { generation } => {
                    let (factory, kind) = (factory.clone(), app.list.kind.clone());
                    let (profile, region) = (app.profile.clone(), app.region.clone());
                    loads.spawn(load_list(
                        factory,
                        profile,
                        region,
                        kind,
                        generation,
                        tx.clone(),
                    ));
                }
                Effect::LoadDetail { generation, id } => {
                    let (factory, kind, tx) = (factory.clone(), app.list.kind.clone(), tx.clone());
                    let (profile, region) = (app.profile.clone(), app.region.clone());
                    loads.spawn(async move {
                        let ctx = factory.ctx(&profile, &region).await;
                        let result = kind.describe(&scoped(&ctx, kind.scope()), &id).await;
                        let _ = tx.send(Input::Msg(Msg::Detail { generation, result }));
                    });
                }
                Effect::RunInTerminal(command) => {
                    input.pause();
                    ratatui::restore();
                    let success = run_command(&command).await;
                    resume_terminal(&mut terminal)?;
                    input.resume();
                    pending.extend(app.handle_msg(Msg::TerminalCommandFinished { success }));
                }
            }
        }
        if !pending.is_empty() {
            continue;
        }

        terminal.draw(|f| render::draw(f, &app))?;
        pending = match rx.recv().await {
            Some(Input::Key(key)) => app.handle_key(key),
            Some(Input::Msg(msg)) => app.handle_msg(msg),
            Some(Input::Redraw) => Vec::new(),
            None => break,
        };
        // Reap finished loads so the set doesn't grow.
        while loads.try_join_next().is_some() {}
    }

    loads.abort_all();
    input.stop();
    ratatui::restore();
    Ok(())
}

/// The context a kind's calls run in: global and pinned services ignore the
/// current region.
fn scoped(ctx: &Ctx, scope: Scope) -> Ctx {
    match scope {
        Scope::Regional => ctx.clone(),
        Scope::Global => ctx.in_region("us-east-1"),
        Scope::Pinned(region) => ctx.in_region(region),
    }
}

/// Streams every page of a list to the app, stopping at the first error.
async fn load_list(
    factory: Arc<ConfigFactory>,
    profile: String,
    region: String,
    kind: Arc<dyn ResourceKind>,
    generation: u64,
    tx: UnboundedSender<Input>,
) {
    let ctx = scoped(&factory.ctx(&profile, &region).await, kind.scope());
    let mut query = ListQuery::default();
    loop {
        let result = kind.list(&ctx, &query).await;
        let next = result.as_ref().ok().and_then(|p| p.next.clone());
        if tx
            .send(Input::Msg(Msg::Page { generation, result }))
            .is_err()
        {
            return;
        }
        match next {
            Some(token) => query.next = Some(token),
            None => return,
        }
    }
}

/// Runs a command in the normal terminal and waits for it.
async fn run_command(command: &[String]) -> bool {
    let Some((program, args)) = command.split_first() else {
        return false;
    };
    println!("$ {}", command.join(" "));
    match tokio::process::Command::new(program)
        .args(args)
        .status()
        .await
    {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("could not run {program}: {err}");
            false
        }
    }
}

/// Re-enters the TUI after [`run_command`], without installing another panic hook.
fn resume_terminal(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout()
        .execute(EnterAlternateScreen)?
        .execute(cursor::Hide)?;
    terminal.clear()
}

/// Reads terminal events on a plain thread (crossterm's reader is blocking). It can
/// be paused so a child process gets the keyboard to itself.
struct InputThread {
    paused: Arc<AtomicBool>,
    /// Set by the thread once it has stopped reading because of `paused`.
    parked: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl InputThread {
    const POLL: Duration = Duration::from_millis(50);

    fn spawn(tx: UnboundedSender<Input>) -> Self {
        let paused = Arc::new(AtomicBool::new(false));
        let parked = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, k, s) = (paused.clone(), parked.clone(), stop.clone());
        thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                if p.load(Ordering::Acquire) {
                    k.store(true, Ordering::Release);
                    thread::sleep(Self::POLL);
                    continue;
                }
                k.store(false, Ordering::Release);
                if !event::poll(Self::POLL).unwrap_or(false) {
                    continue;
                }
                let input = match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => Input::Key(key),
                    Ok(Event::Resize(..)) => Input::Redraw,
                    _ => continue,
                };
                if tx.send(input).is_err() {
                    return;
                }
            }
        });
        Self {
            paused,
            parked,
            stop,
        }
    }

    /// Stops reading and waits until the thread has let go of the terminal.
    fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        while !self.parked.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Release);
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}
