use clap::Parser;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};
use chrono::Utc;
use hostname::get;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use tokio::time::sleep;

/// CLI options
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Comma-separated list of local ports to monitor (e.g., 4317,4318)
    #[arg(short, long)]
    ports: String,

    /// Output JSON file path
    #[arg(short, long)]
    output: Option<String>,

    /// Kafka broker list (e.g., localhost:9092)
    #[arg(long)]
    brokers: Option<String>,

    /// Kafka topic name
    #[arg(long)]
    topic: Option<String>,
}

#[derive(Serialize, Clone)]
struct FlatConnection {
    host: String,
    port: u16,
    unique_ips: Vec<String>,
    count: usize,
    timestamp: String,
}

fn hex_to_ipv4(hex: &str) -> Option<Ipv4Addr> {
    u32::from_str_radix(hex, 16).ok().map(|ip| Ipv4Addr::from(ip.to_le()))
}

fn hex_to_ipv6(hex: &str) -> Option<Ipv6Addr> {
    u128::from_str_radix(hex, 16).ok().map(|ip| Ipv6Addr::from(ip.to_be()))
}

fn parse_proc_net_tcp(path: &str, ports: &HashSet<u16>, is_ipv6: bool) -> HashMap<u16, HashSet<String>> {
    let mut port_map: HashMap<u16, HashSet<String>> = HashMap::new();

    if let Ok(file) = File::open(path) {
        let reader = BufReader::new(file);

        for (i, line) in reader.lines().enumerate() {
            if i == 0 {
                continue;
            }

            if let Ok(line) = line {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() < 4 {
                    continue;
                }

                let local_address = fields[1];
                let remote_address = fields[2];
                let state = fields[3];

                if state != "01" {
                    continue; // Only consider established connections
                }

                let (_, local_port_hex) = local_address.split_once(':').unwrap_or(("", ""));
                let (remote_ip_hex, _) = remote_address.split_once(':').unwrap_or(("", ""));

                if let Ok(local_port) = u16::from_str_radix(local_port_hex, 16) {
                    if ports.contains(&local_port) {
                        let remote_ip = if is_ipv6 {
                            hex_to_ipv6(remote_ip_hex).map(|ip| ip.to_string())
                        } else {
                            hex_to_ipv4(remote_ip_hex).map(|ip| ip.to_string())
                        };

                        if let Some(ip_str) = remote_ip {
                            port_map.entry(local_port).or_default().insert(ip_str);
                        }
                    }
                }
            }
        }
    }

    port_map
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Validate CLI input
    if args.output.is_none() && (args.brokers.is_none() || args.topic.is_none()) {
        eprintln!(
            "Error: Please provide either --output or both --brokers and --topic.\n\
            Example 1: --ports 4317 --output conntracker.json\n\
            Example 2: --ports 4317 --brokers localhost:9092 --topic conntracker\n\
            Example 3: --ports 4317 --output conntracker.json --brokers localhost:9092 --topic conntracker"
        );
        std::process::exit(1);
    }

    let ports: HashSet<u16> = args
        .ports
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .collect();

    let hostname = get().unwrap_or_default().to_string_lossy().to_string();

    let maybe_producer = if let (Some(brokers), Some(_)) = (&args.brokers, &args.topic) {
        Some(
            ClientConfig::new()
                .set("bootstrap.servers", brokers)
                .set("message.timeout.ms", "10000")
                .set("request.timeout.ms", "15000")
                .set("queue.buffering.max.ms", "100")
                .create::<FutureProducer>()
                .expect("Failed to create Kafka producer"),
        )
    } else {
        None
    };

    loop {
        let mut results: HashMap<u16, HashSet<String>> = HashMap::new();

        let tcp4 = parse_proc_net_tcp("/proc/net/tcp", &ports, false);
        let tcp6 = parse_proc_net_tcp("/proc/net/tcp6", &ports, true);

        for (port, ips) in tcp4.into_iter().chain(tcp6) {
            results.entry(port).or_default().extend(ips);
        }

        let timestamp = Utc::now().to_rfc3339();

        let flat_output: Vec<FlatConnection> = results
            .into_iter()
            .map(|(port, ips)| {
                let ip_list: Vec<String> = ips.into_iter().collect();
                FlatConnection {
                    host: hostname.clone(),
                    port,
                    unique_ips: ip_list.clone(),
                    count: ip_list.len(),
                    timestamp: timestamp.clone(),
                }
            })
            .collect();

        // Write to JSON
        if let Some(ref output_path) = args.output {
            if let Ok(json) = serde_json::to_string_pretty(&flat_output) {
                if let Ok(mut file) = File::create(output_path) {
                    let _ = file.write_all(json.as_bytes());
                }
            }
        }

        // Send to Kafka
        if let (Some(producer), Some(topic)) = (&maybe_producer, &args.topic) {
            for entry in &flat_output {
                if let Ok(payload) = serde_json::to_string(&entry) {
                    let key = format!("{}:{}", entry.host, entry.port);
                    let record = FutureRecord::to(topic).payload(&payload).key(&key);

                    match producer.send(record, None).await {
                        Ok(_) => {}
                        Err((err, _)) => eprintln!("Kafka delivery failed: {:?}", err),
                    }
                }
            }
        }

        sleep(Duration::from_secs(10)).await;
    }
}
