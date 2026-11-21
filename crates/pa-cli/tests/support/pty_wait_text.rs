use std::io::Read;
use std::time::{Duration, Instant};

pub(super) fn wait_from(
    file: &mut std::fs::File,
    output: &mut Vec<u8>,
    mark: usize,
    needle: &str,
    what: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if find_subsequence(&output[mark..], needle.as_bytes()).is_some() {
            return;
        }
        let mut buffer = [0u8; 8192];
        let result = file.read(&mut buffer);
        match result {
            Ok(0) | Err(_) => {}
            Ok(n) => output.extend_from_slice(&buffer[..n]),
        }
        if Instant::now() > deadline {
            let text = String::from_utf8_lossy(&output[mark..]);
            panic!("timeout waiting for {what} (needle {needle:?}); pty tail since mark:\n{text}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
