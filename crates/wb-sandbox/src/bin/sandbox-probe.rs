//! Test-only child process for the `wb-sandbox` integration tests. Not shipped.
//!
//! Each subcommand attempts one thing a sandbox should allow or deny and reports the result
//! through its exit code: 0 = it worked, 1 = it was refused, 2 = bad usage, 3 = out of memory.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::exit;
use std::time::Duration;

fn usage() -> ! {
    eprintln!("usage: sandbox-probe read <file> | write <file> | connect <ip:port> <ms> | hog <mib> | exit <code> | sleep <ms> | echo <args>... | env <name>");
    exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| -> &str { args.get(i).map(String::as_str).unwrap_or_else(|| usage()) };
    match arg(0) {
        "read" => match std::fs::File::open(arg(1)).and_then(|mut f| {
            let mut s = String::new();
            f.read_to_string(&mut s).map(|_| s)
        }) {
            Ok(text) => print!("{text}"),
            Err(e) => {
                eprintln!("read-failed: {e}");
                exit(1)
            }
        },
        "write" => match std::fs::File::create(arg(1))
            .and_then(|mut f| f.write_all(b"written by sandbox-probe\n"))
        {
            Ok(()) => println!("write-ok"),
            Err(e) => {
                eprintln!("write-failed: {e}");
                exit(1)
            }
        },
        "connect" => {
            let addr: SocketAddr = arg(1).parse().unwrap_or_else(|_| usage());
            let ms: u64 = arg(2).parse().unwrap_or_else(|_| usage());
            match TcpStream::connect_timeout(&addr, Duration::from_millis(ms)) {
                Ok(_) => println!("connect-ok"),
                Err(e) => {
                    eprintln!("connect-failed: {e}");
                    exit(1)
                }
            }
        }
        "hog" => {
            let mib: usize = arg(1).parse().unwrap_or_else(|_| usage());
            const CHUNK: usize = 8 * 1024 * 1024;
            let mut chunks: Vec<Vec<u8>> = Vec::new();
            let mut held = 0usize;
            while held < mib * 1024 * 1024 {
                let mut chunk: Vec<u8> = Vec::new();
                if chunk.try_reserve_exact(CHUNK).is_err() {
                    eprintln!("alloc-failed after {} MiB", held / (1024 * 1024));
                    exit(3);
                }
                chunk.resize(CHUNK, 0);
                // Touch every page so the memory is really committed and resident.
                for i in (0..CHUNK).step_by(4096) {
                    chunk[i] = 1;
                }
                held += CHUNK;
                chunks.push(chunk);
            }
            let sum: usize = chunks.iter().map(|c| c[0] as usize).sum();
            println!("hog-survived {sum}");
        }
        "exit" => exit(arg(1).parse().unwrap_or_else(|_| usage())),
        "sleep" => {
            std::thread::sleep(Duration::from_millis(
                arg(1).parse().unwrap_or_else(|_| usage()),
            ));
            println!("slept");
        }
        "echo" => println!("{}", args[1..].join("|")),
        "env" => match std::env::var(arg(1)) {
            Ok(v) => println!("{v}"),
            Err(_) => exit(1),
        },
        _ => usage(),
    }
}
