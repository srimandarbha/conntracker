use clap::Parser;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, Ipv6Addr},
    thread,
    time::Duration,
};
use chrono::Utc;
use hostname::get;

/// CLI options
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Comma-separated list of local ports to monitor (e.g., 4317,4318)
    #[arg(short, long)]
    ports: String,

    /// Output JSON file path
    #[arg(short, long)]
    output: String,
}

#[derive(Serialize)]
struct PortEntry {
    port: u16,
    unique_ips: Vec<String>,
    count: usize,
    timestamp: String,
}

#[derive(Serialize)]
struct TrackerOutput {
    host: String,
    connections: Vec<PortEntry>,
}

/// Convert hex to IPv4 address
fn hex_to_ipv4(hex: &str) -> Option<Ipv4Addr> {
    u32::from_str_radix(hex, 16).ok().map(|ip| Ipv4Addr::from(ip.to_le()))
}

/// Convert hex to IPv6 address
fn hex_to_ipv6(hex: &str) -> Option<Ipv6Addr> {
    u128::from_str_radix(hex, 16).ok().map(|ip| Ipv6Addr::from(ip.to_be()))
}

/// Parse /proc/net/tcp or tcp6
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
                    continue; // Only established connections
                }

                let (local_ip_hex, local_port_hex) = local_address.split_once(':').unwrap_or(("", ""));
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

fn main() {
    let args = Args::parse();

    let ports: HashSet<u16> = args
        .ports
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .collect();

    let hostname = get().unwrap_or_default().to_string_lossy().to_string();

    loop {
        let mut results: HashMap<u16, HashSet<String>> = HashMap::new();

        let tcp4 = parse_proc_net_tcp("/proc/net/tcp", &ports, false);
        let tcp6 = parse_proc_net_tcp("/proc/net/tcp6", &ports, true);

        for (port, ips) in tcp4.into_iter().chain(tcp6) {
            results.entry(port).or_default().extend(ips);
        }

        let timestamp = Utc::now().to_rfc3339();


        let connections = results
           .into_iter()
           .map(|(port, ips)| {
                let ip_list: Vec<String> = ips.into_iter().collect();
                let count = ip_list.len();
                PortEntry {
                   port,
                   unique_ips: ip_list,
                   count,
                   timestamp: timestamp.clone(),
                }
            })
            .collect::<Vec<_>>();

        let output = TrackerOutput {
            host: hostname.clone(),
            connections,
        };

        if let Ok(json) = serde_json::to_string_pretty(&output) {
            if let Ok(mut file) = File::create(&args.output) {
                let _ = file.write_all(json.as_bytes());
            }
        }

        thread::sleep(Duration::from_secs(10));
    }
}
