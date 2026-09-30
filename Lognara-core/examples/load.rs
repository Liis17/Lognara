//! HTTP нагрузочный стенд. Токены передаются только через окружение.
use anyhow::{Context, Result};
use lognara_core::{
    model::{format_timestamp, now_nanos},
    wire::{CoreBatch, Event, Group, LogId, LogLevel, Source},
};
use reqwest::Client;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Default)]
struct Measurements {
    errors: AtomicU64,
    search_us: Mutex<Vec<u64>>,
    histogram_us: Mutex<Vec<u64>>,
    visibility_us: Mutex<Vec<u64>>,
}

fn number(name: &str, default: u64) -> Result<u64> {
    Ok(std::env::var(name)
        .ok()
        .map(|text| text.parse())
        .transpose()?
        .unwrap_or(default))
}

fn percentile(values: &[u64], percent: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_unstable();
    Some(values[(values.len() * percent).div_ceil(100).saturating_sub(1)])
}

fn timings(values: &[u64]) -> serde_json::Value {
    json!({"samples":values.len(),"p50_us":percentile(values,50),"p95_us":percentile(values,95),"p99_us":percentile(values,99),"max_us":values.iter().max()})
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("LOGNARA_BENCH_URL").unwrap_or_else(|_| "http://127.0.0.1:7402".into());
    let ingest_token = std::env::var("LOGNARA_INGEST_TOKEN")?;
    let query_token = std::env::var("LOGNARA_QUERY_TOKEN")?;
    let seconds = number("LOGNARA_BENCH_SECONDS", 1800)?;
    let rate = number("LOGNARA_BENCH_RATE", 10_000)?;
    let batch_size = number("LOGNARA_BENCH_BATCH_SIZE", 1000)?;
    anyhow::ensure!(
        seconds > 0 && rate > 0 && batch_size > 0 && batch_size <= 100_000,
        "invalid benchmark settings"
    );
    let report_path =
        std::env::var("LOGNARA_BENCH_REPORT").context("LOGNARA_BENCH_REPORT is required")?;
    let client = Client::builder().timeout(Duration::from_secs(35)).build()?;
    let measurements = Arc::new(Measurements::default());
    let started = Instant::now();
    let stop = started + Duration::from_secs(seconds);
    let mut tasks = vec![];
    for histogram in [false, true] {
        let (client, url, token, measurements) = (
            client.clone(),
            url.clone(),
            query_token.clone(),
            measurements.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(if histogram { 5 } else { 1 }));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while Instant::now() < stop {
                interval.tick().await;
                let now = now_nanos();
                let mut request = json!({"from":format_timestamp(now - 900_000_000_000),"to":format_timestamp(now)});
                if histogram { request["interval_seconds"] = json!(60); }
                else { request["limit"] = json!(100); request["text"] = json!({"query":"benchmark","mode":"all"}); }
                let begin = Instant::now();
                let endpoint = if histogram { "stats/histogram" } else { "logs/search" };
                let result = client.post(format!("{url}/v1/{endpoint}")).bearer_auth(&token).json(&request).send().await;
                match result {
                    Ok(response) if response.status().is_success() => {
                        if response.bytes().await.is_err() { measurements.errors.fetch_add(1, Relaxed); }
                        let times = if histogram { &measurements.histogram_us } else { &measurements.search_us };
                        times.lock().unwrap().push(begin.elapsed().as_micros() as u64);
                    }
                    _ => { measurements.errors.fetch_add(1, Relaxed); }
                }
            }
        }));
    }
    let (sample_tx, mut sample_rx) = mpsc::channel::<(Uuid, i64, Instant)>(32);
    {
        let (client, url, token, measurements) = (
            client.clone(),
            url.clone(),
            query_token,
            measurements.clone(),
        );
        tasks.push(tokio::spawn(async move {
            while let Some((id, timestamp, ack)) = sample_rx.recv().await {
                let request = json!({"from":format_timestamp(timestamp),"to":format_timestamp(timestamp+1),"filters":{"id":[id.to_string()]},"limit":1});
                loop {
                    let result = client.post(format!("{url}/v1/logs/search")).bearer_auth(&token).json(&request).send().await;
                    if let Ok(response) = result && response.status().is_success()
                        && let Ok(value) = response.json::<serde_json::Value>().await
                        && value["events"].as_array().is_some_and(|events| !events.is_empty()) {
                        measurements.visibility_us.lock().unwrap().push(ack.elapsed().as_micros() as u64);
                        break;
                    }
                    if ack.elapsed() > Duration::from_secs(10) { measurements.errors.fetch_add(1, Relaxed); break; }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }));
    }
    let mut tick = tokio::time::interval(Duration::from_secs_f64(batch_size as f64 / rate as f64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut accepted = 0u64;
    let mut offered = 0u64;
    let mut ack_us = vec![];
    let mut uncompressed = 0u64;
    let mut compressed = 0u64;
    let mut next_sample = Instant::now();
    let mut next_report = Instant::now() + Duration::from_secs(30);
    let mut random = 0x5a17_9823_8172_0011_u64;
    while Instant::now() < stop {
        tick.tick().await;
        let timestamp = now_nanos();
        let events: Vec<_> = (0..batch_size)
            .map(|offset| {
                let payload: String = (0..700)
                    .map(|_| {
                        random ^= random << 13;
                        random ^= random >> 7;
                        random ^= random << 17;
                        char::from(b'a' + (random % 26) as u8)
                    })
                    .collect();
                Event {
                    id: LogId(Uuid::now_v7()),
                    timestamp: timestamp + offset as i64,
                    ingested_at: timestamp,
                    action: Some("http.request".into()),
                    level: if offset % 10 == 0 {
                        LogLevel::Error
                    } else {
                        LogLevel::Info
                    },
                    message: format!("benchmark request {} completed", offered + offset),
                    trace_id: None,
                    span_id: None,
                    parent_span_id: None,
                    request_id: None,
                    attributes: HashMap::from([
                        ("payload".into(), json!(payload)),
                        ("elapsed_ms".into(), json!(offset % 1000)),
                    ]),
                }
            })
            .collect();
        let sample_id = events[0].id.0;
        let batch = CoreBatch {
            dropped: 0,
            groups: vec![Group {
                source: Source {
                    environment: Some("benchmark".into()),
                    server: "bench-01".into(),
                    backend: "load".into(),
                    service: "api".into(),
                    service_instance: Some("api-1".into()),
                },
                dropped: 0,
                events,
            }],
        };
        let packed = rmp_serde::to_vec_named(&batch)?;
        let body = zstd::encode_all(&packed[..], 3)?;
        uncompressed += packed.len() as u64;
        compressed += body.len() as u64;
        offered += batch_size;
        let begin = Instant::now();
        let result = client
            .post(format!("{url}/v1/batches"))
            .bearer_auth(&ingest_token)
            .header("content-type", "application/msgpack")
            .header("content-encoding", "zstd")
            .body(body)
            .send()
            .await;
        match result {
            Ok(response) if response.status() == reqwest::StatusCode::NO_CONTENT => {
                accepted += batch_size;
                ack_us.push(begin.elapsed().as_micros() as u64);
                if Instant::now() >= next_sample {
                    if sample_tx
                        .try_send((sample_id, timestamp, Instant::now()))
                        .is_err()
                    {
                        measurements.errors.fetch_add(1, Relaxed);
                    }
                    next_sample = Instant::now() + Duration::from_secs(1);
                }
            }
            _ => {
                measurements.errors.fetch_add(1, Relaxed);
            }
        }
        if Instant::now() >= next_report {
            eprintln!(
                "elapsed={}s accepted={} rate={:.0}/s errors={}",
                started.elapsed().as_secs(),
                accepted,
                accepted as f64 / started.elapsed().as_secs_f64(),
                measurements.errors.load(Relaxed)
            );
            next_report += Duration::from_secs(30);
        }
    }
    let ingest_seconds = started.elapsed().as_secs_f64();
    drop(sample_tx);
    for task in tasks {
        task.await?;
    }
    let report = json!({"target_rate":rate,"requested_seconds":seconds,"ingest_seconds":ingest_seconds,"offered_events":offered,
        "accepted_events":accepted,"accepted_per_second":accepted as f64 / ingest_seconds,"errors":measurements.errors.load(Relaxed),
        "wire_uncompressed_bytes":uncompressed,"wire_compressed_bytes":compressed,"average_wire_event_bytes":uncompressed as f64 / offered as f64,
        "ack":timings(&ack_us),"search":timings(&measurements.search_us.lock().unwrap()),
        "histogram":timings(&measurements.histogram_us.lock().unwrap()),"visibility":timings(&measurements.visibility_us.lock().unwrap())});
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
