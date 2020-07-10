use chrono::prelude::*;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json;
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::time::SystemTime;
use structopt::StructOpt;
use toml;

// TODO:
// Remove unwrap and handle errors
// Support more output formats. Write without completely reading into memory.
// Better init (creating profiles directory, config etc.)
// Refactor code for readability
// Handle paths better, find format! alternate
// Alert about UTF-8 filename assumption

struct Context {
  profiles: Vec<Profile>,
  working_directory: PathBuf,
}

#[derive(Serialize, Deserialize, Debug)]
struct State {
  last_run: u64,
  last_sync: u64,
  last_historyvisit_id: u64,
}

impl State {
  fn from_json(filename: &str) -> Self {
    if !std::path::Path::new(filename).exists() {
      return Self {
        last_run: 0,
        last_sync: 0,
        last_historyvisit_id: 0,
      };
    }

    let file = fs::File::open(filename).unwrap();
    let reader = BufReader::new(file);

    let u: serde_json::Value = serde_json::from_reader(reader).unwrap();
    return serde_json::from_value(u.clone()).unwrap();
  }

  fn to_json(&self, filename: &str) {
    let file = fs::OpenOptions::new()
      .create(true)
      .write(true)
      .open(filename)
      .unwrap();
    let writer = BufWriter::new(file);
    let state = serde_json::to_value(&self).unwrap();
    serde_json::to_writer_pretty(writer, &state).unwrap();
  }
}

#[derive(Debug)]
struct Profile {
  name: String,
  path: PathBuf,
  db_path: PathBuf,
  state: State,
}

#[derive(Debug)]
struct MozPlaces {
  url: String,
}

#[derive(Debug)]
struct MozHistoryVisits {
  id: u32,
  place_id: u32,
  visit_date: i64,
  visit_type: u8,
}

#[derive(Debug, Serialize, Deserialize)]
struct HistoryEntry {
  date: String,
  url: String,
  visit_date: i64,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "Firefox export", about = "Export Firefox data to files")]
struct Opt {
  #[structopt(short = "c", long = "config")]
  config: PathBuf,
}

impl Context {
  fn from_config(filename: PathBuf) -> Context {
    let raw_config: String = fs::read_to_string(filename).unwrap();
    let value = raw_config.parse::<toml::Value>().unwrap();

    let working_directory = PathBuf::from(value["working_directory"].as_str().unwrap());

    let mut context = Context {
      working_directory: working_directory.clone(),
      profiles: vec![],
    };

    for profile in value["profile"].as_table().iter() {
      for (profile_name, path) in profile.iter() {
        context.profiles.push(Profile {
          name: profile_name.to_string(),
          path: PathBuf::from(path["firefox_path"].as_str().unwrap()),
          db_path: PathBuf::from(format!(
            "{}/profiles/{}/places.sqlite",
            &working_directory.to_string_lossy(),
            profile_name
          )),
          state: State::from_json(
            format!(
              "{}/profiles/{}/state.json",
              &working_directory.to_string_lossy(),
              profile_name
            )
            .as_str(),
          ),
        });
      }
    }

    return context;
  }

  fn backup_places(&self) {
    for profile in &self.profiles {
      fs::copy(format!("{}/places.sqlite", profile.path.to_string_lossy()), &profile.db_path).unwrap();
    }
  }
}

impl Profile {
  fn get_place_entry(&self, place_id: u32) -> String {
    let conn = Connection::open(&self.db_path).unwrap();
    let mut stmt = conn
      .prepare("SELECT url FROM moz_places where id = :place_id")
      .unwrap();
    let place_iter = stmt
      .query_map(params![&place_id], |row| Ok(MozPlaces { url: row.get(0)? }))
      .unwrap();

    return place_iter.take(1).next().unwrap().unwrap().url;
  }

  fn get_history(&self, from_id: u64) -> Vec<HistoryEntry> {
    let mut history_entries: Vec<HistoryEntry> = vec![];

    let conn = Connection::open(&self.db_path).unwrap();
    let mut stmt = conn
      .prepare(
        "SELECT id, place_id, visit_date, visit_type FROM moz_historyvisits where id > :from_id",
      )
      .unwrap();
    let history_iter = stmt
      .query_map(params![&(from_id as i64)], |row| {
        Ok(MozHistoryVisits {
          id: row.get(0)?,
          place_id: row.get(1)?,
          visit_date: row.get(2)?,
          visit_type: row.get(3)?,
        })
      })
      .unwrap();

    for visit in history_iter {
      let entry = visit.unwrap();
      history_entries.push(HistoryEntry {
        url: self.get_place_entry(entry.place_id),
        visit_date: entry.visit_date,
        date: Local
          .timestamp(
            // check why timestamp_nanos result in wrong datetime
            Local.timestamp_millis(entry.visit_date / 1000).timestamp(),
            0,
          )
          .to_string(),
      })
    }

    return history_entries;
  }
}

fn write_history_to_file(history: &Vec<HistoryEntry>, filename: &str) {
  let file = fs::OpenOptions::new()
    .create(true)
    .write(true)
    .open(filename)
    .unwrap();
  let writer = BufWriter::new(file);
  let state = serde_json::to_value(history).unwrap();
  serde_json::to_writer_pretty(writer, &state).unwrap();
}

fn main() {
  let opt = Opt::from_args();
  let mut context = Context::from_config(opt.config);

  context.backup_places();
  for profile in &mut context.profiles {
    println!("Getting history entries for profile \"{}\"", profile.name);

    let now = SystemTime::now()
      .duration_since(SystemTime::UNIX_EPOCH)
      .unwrap()
      .as_millis();
    profile.state.last_run = now as u64;
    let history = profile.get_history(profile.state.last_historyvisit_id);

    if history.len() > 0 {
      write_history_to_file(
        &history,
        format!(
          "{}/profiles/{}/history_export_{}.json",
          &context.working_directory.to_string_lossy(),
          profile.name,
          now
        )
        .as_str(),
      );
      println!("Exported {} entries!", history.len());

      profile.state.last_historyvisit_id =
        profile.state.last_historyvisit_id + (history.len() as u64);
      profile.state.last_sync = now as u64;
    } else {
      println!("Nothing to do!");
    }

    profile.state.to_json(
      format!(
        "{}/profiles/{}/state.json",
        &context.working_directory.to_string_lossy(),
        profile.name
      )
      .as_str(),
    )
  }
}
