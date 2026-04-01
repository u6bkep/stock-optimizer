use std::io::{self, Read};

use stock_optimizer::{Config, optimize};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let input = if args.len() > 1 && args[1] != "-" {
        std::fs::read_to_string(&args[1]).unwrap_or_else(|e| {
            eprintln!("Error reading {}: {}", args[1], e);
            std::process::exit(1);
        })
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
            eprintln!("Error reading stdin: {}", e);
            std::process::exit(1);
        });
        buf
    };

    let config: Config = serde_json::from_str(&input).unwrap_or_else(|e| {
        eprintln!("Error parsing JSON config: {}", e);
        eprintln!();
        eprintln!("Expected format:");
        eprintln!(r#"  {{"stock_length": 72, "kerf": 0.125, "parts": [{{"length": 12, "qty": 4}}]}}"#);
        std::process::exit(1);
    });

    match optimize(&config) {
        Ok(solution) => {
            println!("{}", serde_json::to_string_pretty(&solution).unwrap());
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
