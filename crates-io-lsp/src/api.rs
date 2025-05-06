use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;
use crossbeam_channel as mpmc;
use reqwest::blocking::Client;
use serde::Deserialize;
use tracing::info;

const THREADS: usize = 4;

pub struct VersionDB<'a> {
    cache: HashMap<String, Vec<String>>,
    request_tx: mpmc::Sender<String>,
    response_rx: mpsc::Receiver<(String, Vec<String>)>,
    _threads: Vec<thread::ScopedJoinHandle<'a, ()>>,
}

impl<'a> VersionDB<'a> {
    pub fn new(scope: &'a thread::Scope<'a, '_>, endpoint: &'a str, token: Option<&'a str>) -> Self {
        let (threads, request_tx, response_rx) = create_workers(THREADS, scope, move |name: String| {
            let versions = fetch_versions(&name, endpoint, token).unwrap_or_default();
            (name, versions)
        });
        VersionDB {
            cache: HashMap::new(),
            request_tx,
            response_rx,
            _threads: threads,
        }
    }

    pub fn get_versions(&mut self, names: Vec<impl Into<String>>) -> Vec<(String, Vec<String>)> {
        let mut results = Vec::new();
        let mut count = 0;
        for name in names {
            let name: String = name.into();
            if let Some(versions) = self.cache.get(&name) {
                results.push((name, versions.clone()));
            } else {
                self.request_tx.send(name).unwrap();
                count += 1;
            }
        }
        for _ in 0..count {
            let (name, versions) = self.response_rx.recv().unwrap();
            self.cache.insert(name.clone(), versions.clone());
            results.push((name, versions));
        }
        results
    }
}

fn fetch_versions(name: &str, endpoint: &str, token: Option<&str>) -> Result<Vec<String>> {
    let prefix = if name.len() <= 2 {
        name.len().to_string()
    } else if name.len() == 3 {
        format!("{}/{}", name.len(), &name[0..1])
    } else {
        format!("{}/{}", &name[0..2], &name[2..4])
    };
    let mut request = Client::new().get(format!("{endpoint}/{prefix}/{name}"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    let response = request.send()?;
    let mut versions = Vec::new();
    for line in response.text()?.lines() {
        let data: Version = serde_json::from_str(line)?;
        if !data.yanked {
            versions.push(data.vers);
        }
    }
    Ok(versions)
}

#[derive(Deserialize, Debug)]
struct Version {
    vers: String,
    yanked: bool,
}

fn create_workers<'a, S: Send + 'a, R: Send + 'a>(
    count: usize,
    scope: &'a thread::Scope<'a, '_>,
    f: impl Fn(R) -> S + Send + Clone + 'a,
) -> (
    Vec<thread::ScopedJoinHandle<'a, ()>>,
    mpmc::Sender<R>,
    mpsc::Receiver<S>,
) {
    let (request_tx, request_rx) = mpmc::bounded::<R>(count);
    let (versions_tx, versions_rx) = mpsc::channel::<S>();
    let threads = (0..count)
        .map(|_| {
            let request_rx = request_rx.clone();
            let versions_tx = versions_tx.clone();
            let f = f.clone();
            scope.spawn(move || {
                while let Ok(r) = request_rx.recv() {
                    versions_tx.send(f(r)).unwrap();
                }
                info!("Worker finished");
            })
        })
        .collect::<Vec<_>>();
    (threads, request_tx, versions_rx)
}
