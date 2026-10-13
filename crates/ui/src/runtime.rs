//! The terminal loop: draws the app, feeds it keys and background results, and
//! carries out its [`Effect`]s.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use fast_aws_core::{ConfigFactory, Ctx, ListQuery, Parent, ResourceKind, Scope, caller_identity};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::crossterm::{ExecutableCommand, cursor};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::{AbortHandle, JoinSet};

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
    // Kept apart from `loads` so `CancelLoads` never stops the credentials check.
    let mut identity: Option<AbortHandle> = None;

    let mut pending = initial;
    'main: loop {
        for effect in std::mem::take(&mut pending) {
            match effect {
                Effect::Quit => break 'main,
                Effect::CancelLoads => loads.abort_all(),
                Effect::LoadIdentity { generation } => {
                    let (factory, tx) = (factory.clone(), tx.clone());
                    let (profile, region) = (app.profile.clone(), app.region.clone());
                    let task = tokio::spawn(async move {
                        let ctx = factory.ctx(&profile, &region).await;
                        let result = caller_identity(&ctx).await;
                        let _ = tx.send(Input::Msg(Msg::Identity { generation, result }));
                    });
                    if let Some(old) = identity.replace(task.abort_handle()) {
                        old.abort();
                    }
                }
                Effect::LoadList { generation } => {
                    let target = Target::of(&app, &factory);
                    loads.spawn(load_list(target, generation, tx.clone()));
                }
                Effect::LoadDetail { generation, id } => {
                    let (target, tx) = (Target::of(&app, &factory), tx.clone());
                    loads.spawn(async move {
                        let ctx = target.ctx().await;
                        let parent = target.parent.as_ref();
                        let result = target.kind.describe(&ctx, parent, &id).await;
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
    if let Some(task) = identity {
        task.abort();
    }
    input.stop();
    ratatui::restore();
    Ok(())
}

/// What a list or detail load runs against: the current list's kind and parent,
/// in the app's profile and region.
struct Target {
    factory: Arc<ConfigFactory>,
    profile: String,
    region: String,
    kind: Arc<dyn ResourceKind>,
    parent: Option<Parent>,
}

impl Target {
    fn of(app: &App, factory: &Arc<ConfigFactory>) -> Self {
        Self {
            factory: factory.clone(),
            profile: app.profile.clone(),
            region: app.region.clone(),
            kind: app.list.kind.clone(),
            parent: app.list.parent.clone(),
        }
    }

    /// The context the kind's calls run in. Global and pinned services ignore the
    /// current region, and a parent can name its own (an S3 bucket's region).
    async fn ctx(&self) -> Ctx {
        let ctx = self.factory.ctx(&self.profile, &self.region).await;
        let parent_region = self.parent.as_ref().and_then(|p| p.region.as_deref());
        match (self.kind.scope(), parent_region) {
            (Scope::Global, _) => ctx.in_region("us-east-1"),
            (Scope::Pinned(region), _) => ctx.in_region(region),
            (Scope::Regional, Some(region)) => ctx.in_region(region),
            (Scope::Regional, None) => ctx,
        }
    }
}

/// Streams every page of a list to the app, stopping at the first error.
async fn load_list(target: Target, generation: u64, tx: UnboundedSender<Input>) {
    let ctx = target.ctx().await;
    let mut query = ListQuery {
        parent: target.parent.clone(),
        next: None,
    };
    loop {
        let result = target.kind.list(&ctx, &query).await;
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
