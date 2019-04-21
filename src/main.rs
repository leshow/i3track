#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;

mod error;
mod i3;
mod i3log;

pub(crate) use crate::{
    error::TrackErr,
    i3log::{Event, I3Log, Log},
};
use futures::{
    prelude::*,
    sync::mpsc::{self, Sender},
};
use std::{
    fs, io,
    path::Path,
    time::{Duration, Instant},
};
use tokio::{runtime::current_thread::Handle, timer::Delay};

const TIMEOUT_DELAY: u64 = 10;
const LOG_LIMIT: usize = 10;
const LOG_BASE_NAME: &str = "i3tracker";

fn main() -> Result<(), TrackErr> {
    env_logger::init();
    let log_path = setup_log()?;
    // log interval
    info!("Creating listen channel");
    let (tx, rx) = mpsc::channel(50);
    let mut next_id = i3log::initial_event_id(&log_path);
    info!("Next id from logs is {:?}", next_id);

    // catch exit & write to log
    let mut rt = tokio::runtime::current_thread::Runtime::new().expect("Failed building runtime");
    rt.spawn(sigint(tx.clone()));

    // spawn listen loop
    {
        let tx = tx.clone();
        if let Err(e) = i3::listen_loop(tx, rt.handle()) {
            error!("{:?}", e);
        }
    }
    let mut writer = i3log::writer(&log_path)?;
    let mut prev_i3log: Option<I3Log> = None;
    // consume events
    let handle: Handle = rt.handle();

    let f2 = rx.for_each(move |event| {
        match event {
            Event::I3(e) => {
                if let Some(ref prev) = prev_i3log {
                    Log::new(next_id, prev)
                        .write(&mut writer)
                        .expect("write failed");
                    next_id += 1;
                }

                handle
                    .spawn(timeout(tx.clone(), next_id))
                    .expect("Spawn timeout failed");
                prev_i3log = Some(e);
            }
            Event::Tick(id) => {
                if next_id != id {
                    return Ok(());
                }
                if let Some(ref prev) = prev_i3log {
                    info!("Tick - writing log");
                    Log::new(next_id, prev)
                        .write(&mut writer)
                        .expect("write failed!");
                    next_id += 1;
                    prev_i3log = Some(prev.new_start());
                }
                handle
                    .spawn(timeout(tx.clone(), next_id))
                    .expect("Spawn timeout failed");
            }
            Event::Flush => {
                if let Some(ref prev) = prev_i3log {
                    Log::new(next_id, prev)
                        .write(&mut writer)
                        .expect("write failed");
                }
                std::process::exit(0);
            }
        }
        Ok(())
    });
    rt.spawn(f2);
    rt.run().expect("Failed runtime");
    Ok(())
}

fn timeout(tx: Sender<Event>, id: u32) -> impl Future<Item = (), Error = ()> {
    Delay::new(Instant::now() + Duration::from_secs(TIMEOUT_DELAY))
        .map_err(|_| ())
        .and_then(move |_| tx.send(Event::Tick(id)).map_err(|_| ()))
        .map(|_| ())
        .map_err(|_| ())
}

fn sigint(tx: Sender<Event>) -> impl Future<Item = (), Error = ()> {
    tokio_signal::ctrl_c()
        .flatten_stream()
        .for_each(move |_| {
            tx.clone()
                .send(Event::Flush)
                .map(|_| ())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        })
        .map_err(|_| ())
}

fn setup_log() -> Result<impl AsRef<Path>, TrackErr> {
    // get data dir
    let xdg_dir = xdg::BaseDirectories::with_prefix(LOG_BASE_NAME)?;
    let home = xdg_dir.get_data_home();
    info!("Setting up log in {:?}", home.as_path());
    let cur_log = rotate(home.as_path(), LOG_LIMIT)?;
    info!("Current log is {:?}", cur_log);

    Ok(xdg_dir.place_data_file(format!("{}{}.{}", LOG_BASE_NAME, ".log", cur_log))?)
}

fn rotate<P: AsRef<Path>>(dir: P, num: usize) -> Result<usize, TrackErr> {
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let found_log = path
            .file_stem()
            .map(|h| {
                h.to_str()
                    .map(|g| g.starts_with(LOG_BASE_NAME))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if found_log {
            let modif = path.metadata()?.modified()?.elapsed()?.as_secs();
            files.push((path, modif));
        }
    }

    if files.len() >= num {
        files.sort_by(|&(_, a), &(_, ref b)| a.cmp(b));

        if let Some((last, _)) = files.first() {
            if let Some(Some(Ok(n))) = last
                .extension()
                .map(|c| c.to_str().map(|c| c.to_owned().parse::<usize>()))
            {
                return Ok(n + 1);
            }
        }
        Ok(0)
    } else {
        Ok(files.len())
    }
}
